use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::delta::DeltaOp;

// ── Log entry ─────────────────────────────────────────────────────────────────

/// One mutation event recorded in the persistent log.
///
/// Stores both the delta ops (for in-memory RESUME re-population) and the
/// full resulting value (for trivial state reconstruction on recovery — no
/// need to re-apply ops to an initial `Null`; just use the `value` from the
/// last entry for each key).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub version: u64,
    pub key:     String,
    pub ops:     Vec<DeltaOp>,
    /// Full state value after this mutation.
    pub value:   serde_json::Value,
}

// ── On-disk record format ─────────────────────────────────────────────────────
//
//   [ u32 LE: payload_len ][ payload_len bytes: rmp_serde(LogEntry) ]
//
//   A truncated final record — payload_len bytes not available in the file —
//   signals a crash-during-write.  `replay_segment` stops there and truncates
//   the file to the last fully-written byte so future appends stay clean.

fn write_entry(w: &mut impl Write, entry: &LogEntry) -> anyhow::Result<()> {
    let payload = rmp_serde::to_vec(entry)?;
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(&payload)?;
    Ok(())
}

fn read_entry(r: &mut impl Read) -> anyhow::Result<Option<LogEntry>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    // UnexpectedEof here → truncated record → caller treats as Err → stops replay
    r.read_exact(&mut payload)?;
    Ok(Some(rmp_serde::from_slice(&payload)?))
}

// ── Segment helpers ───────────────────────────────────────────────────────────

const SEGMENT_PREFIX: &str = "seg_";
const SEGMENT_SUFFIX: &str = ".log";
/// Maximum entries per segment file before rotation.
pub const MAX_ENTRIES_PER_SEGMENT: usize = 100_000;
/// Compact when this many segments are on disk (including the current write segment).
const MAX_SEGMENTS_BEFORE_COMPACT: usize = 3;

fn segment_path(log_dir: &Path, seg_num: u64) -> PathBuf {
    log_dir.join(format!("{SEGMENT_PREFIX}{seg_num:010}{SEGMENT_SUFFIX}"))
}

/// List all segment numbers present in `log_dir`, sorted ascending.
fn list_segments(log_dir: &Path) -> anyhow::Result<Vec<u64>> {
    let mut nums = Vec::new();
    for e in fs::read_dir(log_dir)? {
        let name = e?.file_name().into_string().unwrap_or_default();
        if let Some(rest) = name.strip_prefix(SEGMENT_PREFIX)
            && let Some(num_str) = rest.strip_suffix(SEGMENT_SUFFIX)
                && let Ok(n) = num_str.parse::<u64>() {
                    nums.push(n);
                }
    }
    nums.sort_unstable();
    Ok(nums)
}

/// Read all valid entries from `path`.
///
/// If the file ends with a partial record (crash-during-write), it is
/// truncated to the last fully-written byte before returning.
fn replay_segment(path: &Path) -> anyhow::Result<Vec<LogEntry>> {
    let mut entries   = Vec::new();
    let mut valid_pos = 0u64;

    {
        let mut r = BufReader::new(File::open(path)?);
        loop {
            let pos_before = r.stream_position()?;
            match read_entry(&mut r) {
                Ok(Some(e))  => { valid_pos = r.stream_position()?; entries.push(e); }
                Ok(None)     => break,  // clean EOF between records
                Err(e) => {
                    warn!("Truncated log record at byte {pos_before} in {path:?}: {e}");
                    break;
                }
            }
        }
    }

    // Truncate to last fully-written record (no-op when the file is clean).
    let actual = fs::metadata(path)?.len();
    if actual != valid_pos {
        warn!("Truncating {path:?} from {actual} to {valid_pos} bytes (crash recovery)");
        OpenOptions::new().write(true).open(path)?.set_len(valid_pos)?;
    }

    Ok(entries)
}

// ── SegmentedLog ──────────────────────────────────────────────────────────────

/// Append-only segmented on-disk log.
///
/// Segment files are named `seg_{n:010}.log` inside the configured directory.
/// A new segment is created when the current one reaches `MAX_ENTRIES_PER_SEGMENT`.
///
/// **Durability**: each `append` flushes the `BufWriter` to the kernel page
/// cache — sufficient to survive a process crash.  For power-loss or OS-crash
/// durability, un-comment the `sync_data()` call inside `append`.
pub struct SegmentedLog {
    log_dir:            PathBuf,
    current_seg:        u64,
    writer:             BufWriter<File>,
    pub(crate) entry_count:   usize,
    segment_count:      usize,
}

impl SegmentedLog {
    /// Open (or create) the log directory.
    ///
    /// Returns `(SegmentedLog, all_replayed_entries_in_disk_order)`.  Pass the
    /// entries to `StateStore::open` to reconstruct in-memory state before
    /// accepting connections.
    pub fn open(log_dir: &Path) -> anyhow::Result<(Self, Vec<LogEntry>)> {
        fs::create_dir_all(log_dir)?;

        let segments = list_segments(log_dir)?;
        let mut all_entries:    Vec<LogEntry> = Vec::new();
        let mut last_seg_count: usize         = 0;

        for (i, &seg_num) in segments.iter().enumerate() {
            let path   = segment_path(log_dir, seg_num);
            let chunk  = replay_segment(&path)?;
            info!("Log: replayed {} entries from {path:?}", chunk.len());
            if i + 1 == segments.len() { last_seg_count = chunk.len(); }
            all_entries.extend(chunk);
        }

        // Open (or create) the current segment for appending.
        let current_seg = segments.last().copied().unwrap_or(0);
        let seg_path    = segment_path(log_dir, current_seg);
        let file        = OpenOptions::new().create(true).append(true).open(&seg_path)?;

        info!(
            "Log ready in {log_dir:?}: {} total entries, segment {current_seg} \
             ({last_seg_count}/{MAX_ENTRIES_PER_SEGMENT} entries)",
            all_entries.len()
        );

        Ok((
            SegmentedLog {
                log_dir:       log_dir.to_owned(),
                current_seg,
                writer:        BufWriter::new(file),
                entry_count:   last_seg_count,
                segment_count: segments.len().max(1),
            },
            all_entries,
        ))
    }

    /// Write one entry.  Rotates to a new segment file when the current one is full.
    pub fn append(&mut self, entry: &LogEntry) -> anyhow::Result<()> {
        if self.entry_count >= MAX_ENTRIES_PER_SEGMENT {
            self.rotate()?;
        }
        write_entry(&mut self.writer, entry)?;
        self.writer.flush()?;           // flush BufWriter → kernel page cache
        // Uncomment for power-loss / OS-crash durability (~1–5 ms per write on SSD):
        // self.writer.get_mut().sync_data()?;
        self.entry_count += 1;
        Ok(())
    }

    fn rotate(&mut self) -> anyhow::Result<()> {
        self.writer.flush()?;
        self.current_seg += 1;
        let path = segment_path(&self.log_dir, self.current_seg);
        let file = OpenOptions::new().create_new(true).append(true).open(&path)?;
        self.writer = BufWriter::new(file);
        self.entry_count = 0;
        self.segment_count += 1;
        info!("Log: rotated to new segment {path:?}");
        Ok(())
    }

    /// Returns `true` when compaction should be triggered.
    pub fn needs_compact(&self) -> bool {
        self.segment_count > MAX_SEGMENTS_BEFORE_COMPACT
    }

    /// Compact the log.
    ///
    /// Writes `snapshot` (one entry per live key, `ops = []`) to a new segment,
    /// opens another fresh segment for subsequent writes, then deletes all
    /// pre-snapshot segments.
    ///
    /// Snapshot entries use `ops = []` so that, after the next restart, the
    /// in-memory `DeltaLog` starts fresh.  Any RESUME request whose
    /// `since_version` predates the snapshot will receive an empty delta list
    /// and fall back to `STATE_INIT` — the correct full-snapshot fallback.
    pub fn compact(&mut self, snapshot: &[LogEntry]) -> anyhow::Result<()> {
        // Snapshot the set of segments that exist RIGHT NOW (before we add new ones).
        let old_segs = list_segments(&self.log_dir)?;

        // Flush the current write buffer.
        self.writer.flush()?;

        // Write the snapshot to a new segment.
        self.current_seg += 1;
        let snap_path = segment_path(&self.log_dir, self.current_seg);
        {
            let snap_file = OpenOptions::new().create_new(true).write(true).open(&snap_path)?;
            let mut snap_writer = BufWriter::new(snap_file);
            for entry in snapshot {
                write_entry(&mut snap_writer, entry)?;
            }
            snap_writer.flush()?;
        }

        // Open a fresh segment for all subsequent live writes.
        self.current_seg += 1;
        let new_path = segment_path(&self.log_dir, self.current_seg);
        let new_file = OpenOptions::new().create_new(true).append(true).open(&new_path)?;
        self.writer = BufWriter::new(new_file);
        self.entry_count = 0;
        self.segment_count = 2; // snapshot segment + live write segment

        // Delete all pre-snapshot segments.
        for seg_num in old_segs {
            let path = segment_path(&self.log_dir, seg_num);
            if let Err(e) = fs::remove_file(&path) {
                warn!("Compaction: failed to remove old segment {path:?}: {e}");
            }
        }

        info!(
            "Log: compacted — snapshot in {:?}, writing to {:?} ({} keys)",
            snap_path, new_path, snapshot.len()
        );
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::DeltaOp;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("tmpdir")
    }

    fn entry(key: &str, version: u64, val: serde_json::Value) -> LogEntry {
        LogEntry {
            version,
            key: key.to_string(),
            ops: vec![DeltaOp::add("/", val.clone())],
            value: val,
        }
    }

    // ── Write → replay ────────────────────────────────────────────────────────

    #[test]
    fn write_and_replay_single_entry() {
        let dir = tmp();
        {
            let (mut log, replayed) = SegmentedLog::open(dir.path()).unwrap();
            assert!(replayed.is_empty());
            log.append(&entry("k1", 1, serde_json::json!({"x": 1}))).unwrap();
        }
        let (_log, replayed) = SegmentedLog::open(dir.path()).unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].key, "k1");
        assert_eq!(replayed[0].version, 1);
        assert_eq!(replayed[0].value, serde_json::json!({"x": 1}));
    }

    #[test]
    fn write_and_replay_many_keys() {
        let dir = tmp();
        {
            let (mut log, _) = SegmentedLog::open(dir.path()).unwrap();
            for i in 0u64..100 {
                log.append(&entry(&format!("key.{i}"), i + 1, serde_json::json!(i))).unwrap();
            }
        }
        let (_log, replayed) = SegmentedLog::open(dir.path()).unwrap();
        assert_eq!(replayed.len(), 100);
        // Versions are in order.
        for (i, e) in replayed.iter().enumerate() {
            assert_eq!(e.version, i as u64 + 1);
        }
    }

    // ── Crash recovery (truncated final record) ───────────────────────────────

    #[test]
    fn truncated_last_record_is_dropped_and_file_healed() {
        let dir = tmp();
        {
            let (mut log, _) = SegmentedLog::open(dir.path()).unwrap();
            log.append(&entry("a", 1, serde_json::json!(1))).unwrap();
            log.append(&entry("b", 2, serde_json::json!(2))).unwrap();
        }

        // Corrupt the file by truncating its last 3 bytes.
        let seg = segment_path(dir.path(), 0);
        let original_len = std::fs::metadata(&seg).unwrap().len();
        let f = std::fs::OpenOptions::new().write(true).open(&seg).unwrap();
        f.set_len(original_len - 3).unwrap();

        // Replay: only the first entry should survive; file is healed.
        let (_log, replayed) = SegmentedLog::open(dir.path()).unwrap();
        assert_eq!(replayed.len(), 1, "truncated record must be dropped");
        assert_eq!(replayed[0].key, "a");

        // Subsequent append must succeed (file was healed).
        let (mut log2, _) = SegmentedLog::open(dir.path()).unwrap();
        log2.append(&entry("c", 3, serde_json::json!(3))).unwrap();
        let (_log3, replayed2) = SegmentedLog::open(dir.path()).unwrap();
        assert_eq!(replayed2.len(), 2);
        assert_eq!(replayed2[1].key, "c");
    }

    // ── Segment rotation ──────────────────────────────────────────────────────

    /// Verify rotation by manually driving `entry_count` past the cap via
    /// repeated opens — each open re-reads last_seg_count from disk, so we
    /// can simulate a multi-session scenario without writing 100 000 entries.
    #[test]
    fn rotation_creates_new_segment_file() {
        let dir = tmp();

        // Fill segment 0 to exactly the limit, then write one more entry.
        {
            let (mut log, _) = SegmentedLog::open(dir.path()).unwrap();
            // Fake the counter so rotate() triggers on the very next append.
            log.entry_count = MAX_ENTRIES_PER_SEGMENT;
            log.append(&entry("trigger", 1, serde_json::json!("rotate"))).unwrap();
        }

        let segs = list_segments(dir.path()).unwrap();
        assert_eq!(segs.len(), 2, "should have rotated to segment 1");

        // The one real entry survives in the new segment.
        let (_log, replayed) = SegmentedLog::open(dir.path()).unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].key, "trigger");
    }

    // ── Compaction ────────────────────────────────────────────────────────────

    #[test]
    fn compaction_reduces_to_two_segments_and_data_survives() {
        let dir = tmp();
        let (mut log, _) = SegmentedLog::open(dir.path()).unwrap();

        // Write enough to trigger 4 segments (above MAX_SEGMENTS_BEFORE_COMPACT=3).
        let total = (MAX_ENTRIES_PER_SEGMENT * 3 + 1) as u64;
        for i in 0..total {
            log.append(&entry(&format!("k{i}"), i + 1, serde_json::json!(i))).unwrap();
        }

        // Build a snapshot of last-write-wins for each key (all distinct here).
        let snapshot: Vec<LogEntry> = (0..total)
            .map(|i| LogEntry {
                version: i + 1,
                key:     format!("k{i}"),
                ops:     vec![],                    // snapshot entry
                value:   serde_json::json!(i),
            })
            .collect();

        log.compact(&snapshot).unwrap();

        let segs_after = list_segments(dir.path()).unwrap();
        assert_eq!(segs_after.len(), 2, "compact leaves snapshot + live segment");

        // Replay gives back snapshot entries.
        let (_log, replayed) = SegmentedLog::open(dir.path()).unwrap();
        assert_eq!(replayed.len(), total as usize);
    }

    // ── StateStore round-trip ─────────────────────────────────────────────────

    #[test]
    fn statestore_open_reconstructs_state() {
        let dir = tmp();
        {
            let store = crate::state::StateStore::open(dir.path()).unwrap();
            store.apply("game.score",  serde_json::json!({"score": 42}));
            store.apply("game.player", serde_json::json!({"name": "Alice"}));
            store.apply("game.score",  serde_json::json!({"score": 99})); // overwrite
        }
        // Reopen and verify last-write-wins reconstruction.
        let store = crate::state::StateStore::open(dir.path()).unwrap();
        let score  = store.get("game.score").expect("score");
        let player = store.get("game.player").expect("player");
        assert_eq!(score.value,  serde_json::json!({"score": 99}));
        assert_eq!(player.value, serde_json::json!({"name": "Alice"}));
    }

    #[test]
    fn statestore_deltas_survive_restart() {
        let dir = tmp();
        {
            let store = crate::state::StateStore::open(dir.path()).unwrap();
            store.apply("x", serde_json::json!(1));
            store.apply("x", serde_json::json!(2));
            store.apply("x", serde_json::json!(3));
        }
        let store = crate::state::StateStore::open(dir.path()).unwrap();
        // All three delta versions must be replayable for RESUME.
        let deltas = store.deltas_since("x", 0);
        assert_eq!(deltas.len(), 3, "all deltas should survive restart");
        assert_eq!(deltas[2].0, store.get("x").unwrap().version);
    }
}
