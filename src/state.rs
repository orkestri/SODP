use std::path::Path;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use dashmap::DashMap;
use serde_json::Value;

use crate::delta::{diff, DeltaOp};
use crate::log::{LogEntry, SegmentedLog};

#[derive(Debug, Clone)]
pub struct StateEntry {
    pub version: u64,
    pub value: Value,
}

/// Maximum delta log entries retained per key.
/// Older entries are evicted on each append; a RESUME that references an evicted
/// version receives an empty replay and falls back to STATE_INIT automatically.
const MAX_DELTA_LOG_ENTRIES_PER_KEY: usize = 1_000;

// ── Delta event log ───────────────────────────────────────────────────────────

/// Per-key append-only log of every non-empty delta, capped at
/// `MAX_DELTA_LOG_ENTRIES_PER_KEY` entries per key.
///
/// Used to replay missed changes when a client reconnects and sends RESUME.
/// When the cap is reached the oldest entries are evicted.  A RESUME that
/// references an evicted version receives an empty replay and falls back to
/// `STATE_INIT` automatically — clients already handle this transparently.
struct DeltaLog {
    /// state_key → [(version, ops), …] in insertion (version) order.
    entries: DashMap<String, Vec<(u64, Vec<DeltaOp>)>>,
}

impl DeltaLog {
    fn new() -> Self {
        DeltaLog { entries: DashMap::new() }
    }

    fn append(&self, key: &str, version: u64, ops: Vec<DeltaOp>) {
        let mut entry = self.entries.entry(key.to_string()).or_default();
        entry.push((version, ops));
        // Evict the oldest entries when over the cap.
        if entry.len() > MAX_DELTA_LOG_ENTRIES_PER_KEY {
            let excess = entry.len() - MAX_DELTA_LOG_ENTRIES_PER_KEY;
            entry.drain(..excess);
        }
    }

    /// All stored deltas for `key` with version > `since_version`, in order.
    ///
    /// Returns an empty `Vec` when the key has never been written or when the
    /// client is already up to date.  Never returns `None` in this unbounded
    /// variant — all non-empty deltas since the server started are retained.
    fn since(&self, key: &str, since_version: u64) -> Vec<(u64, Vec<DeltaOp>)> {
        match self.entries.get(key) {
            None => vec![],
            Some(entries) => entries
                .iter()
                .filter(|(v, _)| *v > since_version)
                .cloned()
                .collect(),
        }
    }
}

// ── State store ───────────────────────────────────────────────────────────────

/// Concurrent, versioned, in-memory state store.
///
/// Each `apply` atomically:
///   1. Increments the global version counter.
///   2. Computes the structural diff between old and new values.
///   3. Replaces the stored value.
///   4. Appends any non-empty ops to the per-key delta log.
///   5. Returns `(new_version, delta_ops)` for fanout.
pub struct StateStore {
    entries:        DashMap<String, StateEntry>,
    global_version: AtomicU64,
    log:            DeltaLog,
    /// On-disk persistent log.  `None` in ephemeral (in-memory-only) mode.
    plog:           Option<Mutex<SegmentedLog>>,
}

impl StateStore {
    pub fn new() -> Arc<Self> {
        Arc::new(StateStore {
            entries:        DashMap::new(),
            global_version: AtomicU64::new(0),
            log:            DeltaLog::new(),
            plog:           None,
        })
    }

    /// Open a persistent StateStore backed by a segmented on-disk log.
    ///
    /// Replays all segments in order, rebuilding the in-memory state and
    /// per-key delta log.  Entries are sorted by version before applying so
    /// that the in-memory state reflects the true chronological order even if
    /// concurrent appends to the log file produced an out-of-order tail.
    pub fn open(log_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let (plog, mut all_entries) = SegmentedLog::open(log_dir)?;

        // Sort by version — segments are ordered, but the final segment may
        // have a partial record from a crash; sorting is a cheap safety net.
        all_entries.sort_unstable_by_key(|e| e.version);

        let entries        = DashMap::new();
        let delta_log      = DeltaLog::new();
        let max_version    = all_entries.last().map(|e| e.version).unwrap_or(0);

        for entry in all_entries {
            // Rebuild in-memory StateEntry (last write per key wins — already sorted).
            entries.insert(entry.key.clone(), StateEntry {
                version: entry.version,
                value:   entry.value,
            });
            // Rebuild per-key delta log for RESUME replay.
            if !entry.ops.is_empty() {
                delta_log.append(&entry.key, entry.version, entry.ops);
            }
        }

        Ok(Arc::new(StateStore {
            entries,
            global_version: AtomicU64::new(max_version),
            log:            delta_log,
            plog:           Some(Mutex::new(plog)),
        }))
    }

    /// Number of distinct keys currently held in the store.
    pub fn key_count(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, key: &str) -> Option<StateEntry> {
        self.entries.get(key).map(|e| e.clone())
    }

    /// Apply a new value to `key`. Returns `(version, ops)`.
    ///
    /// If the key does not yet exist, ops will contain a single ADD("/", value).
    pub fn apply(&self, key: &str, new_value: Value) -> (u64, Vec<DeltaOp>) {
        let version = self.global_version.fetch_add(1, Ordering::SeqCst) + 1;

        // Compute diff while holding only a read lock on the entry.
        // The DashMap Ref is dropped before the write below.
        let ops = match self.entries.get(key) {
            Some(existing) => diff(&existing.value, &new_value),
            None => vec![DeltaOp::add("/", new_value.clone())],
        };

        self.entries.insert(key.to_string(), StateEntry { version, value: new_value.clone() });

        // Persist non-empty deltas to the in-memory log (always) and to the
        // on-disk segmented log when running in persistent mode.
        if !ops.is_empty() {
            self.log.append(key, version, ops.clone());

            if let Some(plog) = &self.plog {
                let log_entry = LogEntry { version, key: key.to_string(), ops: ops.clone(), value: new_value };
                let needs_compact = {
                    let mut guard = plog.lock().expect("persistent log mutex poisoned");
                    if let Err(e) = guard.append(&log_entry) {
                        tracing::error!("Failed to write log entry: {e}");
                    }
                    guard.needs_compact()
                };
                if needs_compact {
                    self.compact_plog();
                }
            }
        }

        (version, ops)
    }

    /// Remove `key` from the store entirely.
    ///
    /// Returns `Some((version, ops))` with a single `REMOVE "/"` op if the key
    /// existed; `None` if it was already absent (no-op, no version bump).
    pub fn delete(&self, key: &str) -> Option<(u64, Vec<DeltaOp>)> {
        self.entries.remove(key)?;
        let version = self.global_version.fetch_add(1, Ordering::SeqCst) + 1;
        let ops = vec![DeltaOp::remove("/")];

        self.log.append(key, version, ops.clone());

        if let Some(plog) = &self.plog {
            let log_entry = LogEntry {
                version,
                key:   key.to_string(),
                ops:   ops.clone(),
                value: Value::Null,
            };
            let needs_compact = {
                let mut guard = plog.lock().expect("persistent log mutex poisoned");
                if let Err(e) = guard.append(&log_entry) {
                    tracing::error!("Failed to write delete log entry: {e}");
                }
                guard.needs_compact()
            };
            if needs_compact {
                self.compact_plog();
            }
        }

        Some((version, ops))
    }

    /// All logged deltas for `key` with version > `since_version`, in order.
    ///
    /// Used by `handle_resume` to replay missed changes to a reconnecting client.
    pub fn deltas_since(&self, key: &str, since_version: u64) -> Vec<(u64, Vec<DeltaOp>)> {
        self.log.since(key, since_version)
    }

    pub fn current_version(&self) -> u64 {
        self.global_version.load(Ordering::SeqCst)
    }

    /// Populate the store from an external source (e.g. Redis on startup).
    ///
    /// For each entry, the new value wins only if its `version` is strictly
    /// greater than the locally-stored version for the same key (Redis wins
    /// over stale disk state).  The global version counter is advanced to at
    /// least the maximum version seen in the supplied entries.
    pub fn load_entries(&self, entries: Vec<(String, u64, serde_json::Value)>) {
        let mut max_version = 0u64;
        for (key, version, value) in entries {
            max_version = max_version.max(version);
            let current_version = self.entries.get(&key).map(|e| e.version).unwrap_or(0);
            if version > current_version {
                self.entries.insert(key, StateEntry { version, value });
            }
        }
        // Advance the global version so the next mutation gets a higher number.
        let current_global = self.global_version.load(Ordering::SeqCst);
        if max_version > current_global {
            self.global_version.store(max_version, Ordering::SeqCst);
        }
    }

    /// Snapshot all live keys and compact the on-disk log.
    ///
    /// Snapshot entries use `ops = []` so the in-memory `DeltaLog` starts
    /// fresh after the next restart.  Pre-compaction RESUME requests will
    /// receive no replayed deltas and fall back to `STATE_INIT` automatically.
    fn compact_plog(&self) {
        let Some(plog) = &self.plog else { return };

        // Build one LogEntry per live key.  ops = [] marks these as snapshot
        // entries — no DeltaLog population on replay.
        let snapshot: Vec<LogEntry> = self.entries
            .iter()
            .map(|e| LogEntry {
                version: e.version,
                key:     e.key().clone(),
                ops:     vec![],
                value:   e.value.clone(),
            })
            .collect();

        if let Err(e) = plog.lock().expect("persistent log mutex poisoned").compact(&snapshot) {
            tracing::error!("Log compaction failed: {e}");
        }
    }
}
