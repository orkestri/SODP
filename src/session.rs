use std::collections::HashMap;
use std::time::Instant;
use uuid::Uuid;
use serde_json::Value;

// ── Watch entry ───────────────────────────────────────────────────────────────

/// Tracks a single active WATCH subscription within a session.
#[derive(Debug)]
pub struct WatchEntry {
    pub state_key: String,
    pub last_seq:  u64,
}

// ── Presence entry ────────────────────────────────────────────────────────────

/// Records a session-owned state path registered via `state.presence`.
/// When the session ends for any reason, the server automatically removes
/// this path from the state and broadcasts the resulting DELTA.
#[derive(Debug)]
pub struct PresenceEntry {
    pub state_key: String,
    pub path:      String,
}

// ── Rate limiter ──────────────────────────────────────────────────────────────

/// Fixed 1-second window rate limiter.
///
/// Counts requests in the current second.  When `allow()` is called after the
/// window has elapsed the counter resets.  The approach is simple and correct
/// for the common case; it allows up to 2× `max_per_sec` requests across a
/// window boundary, which is acceptable for SODP's write/watch paths.
#[derive(Debug)]
pub struct RateLimiter {
    max_per_sec:  u32,
    count:        u32,
    window_start: Instant,
}

impl RateLimiter {
    pub fn new(max_per_sec: u32) -> Self {
        RateLimiter { max_per_sec, count: 0, window_start: Instant::now() }
    }

    /// Returns `true` if the request is within the rate limit; `false` to reject.
    pub fn allow(&mut self) -> bool {
        if self.window_start.elapsed().as_secs() >= 1 {
            self.count        = 0;
            self.window_start = Instant::now();
        }
        if self.count < self.max_per_sec {
            self.count += 1;
            true
        } else {
            false
        }
    }
}

// ── Session ───────────────────────────────────────────────────────────────────

/// Per-connection session state. Lives entirely within one connection task — not shared.
#[derive(Debug)]
pub struct Session {
    pub id:  String,
    /// Authenticated subject (JWT `sub` claim). `None` in unauthenticated mode.
    pub sub: Option<String>,
    /// All extra JWT claims (everything except `sub` and `exp`).
    /// Used by the ACL engine for role/group/tenant checks.
    /// Empty object when JWT auth is disabled or claims are absent.
    pub claims: Value,
    /// stream_id → subscription metadata
    pub watches: HashMap<u32, WatchEntry>,
    /// Session-owned state paths (auto-removed on session close).
    pub presence: Vec<PresenceEntry>,
    /// Write (CALL) rate limiter.  `None` = unlimited.
    pub write_limiter: Option<RateLimiter>,
    /// Watch / resume rate limiter.  `None` = unlimited.
    pub watch_limiter: Option<RateLimiter>,
    next_stream_id: u32,
    seq_counter:    u64,
}

impl Session {
    /// Create a new session with optional per-second rate limits.
    /// Pass `None` for unlimited.
    pub fn new(write_limit: Option<u32>, watch_limit: Option<u32>) -> Self {
        Session {
            id:             Uuid::new_v4().to_string(),
            sub:            None,
            claims:         Value::Object(serde_json::Map::new()),
            watches:        HashMap::new(),
            presence:       Vec::new(),
            write_limiter:  write_limit.map(RateLimiter::new),
            watch_limiter:  watch_limit.map(RateLimiter::new),
            next_stream_id: 10, // stream 1 is control; subscription streams start at 10
            seq_counter:    0,
        }
    }

    /// Monotonically increasing sequence number for outbound frames.
    pub fn next_seq(&mut self) -> u64 {
        self.seq_counter += 1;
        self.seq_counter
    }

    /// Allocate the next stream ID for a new WATCH subscription.
    pub fn allocate_stream(&mut self) -> u32 {
        let id = self.next_stream_id;
        self.next_stream_id += 1;
        id
    }

    pub fn add_watch(&mut self, stream_id: u32, state_key: String) {
        self.watches.insert(stream_id, WatchEntry { state_key, last_seq: 0 });
    }

    pub fn remove_watch(&mut self, stream_id: u32) -> Option<WatchEntry> {
        self.watches.remove(&stream_id)
    }

    pub fn ack_stream(&mut self, stream_id: u32, seq: u64) {
        if let Some(w) = self.watches.get_mut(&stream_id) {
            w.last_seq = seq;
        }
    }

    /// Return the stream_id for an active WATCH on `state_key`, or `None`.
    pub fn watch_stream_id(&self, state_key: &str) -> Option<u32> {
        self.watches.iter().find(|(_, e)| e.state_key == state_key).map(|(id, _)| *id)
    }

    /// Register a session-owned presence path.  Deduplicates by (state_key, path).
    pub fn add_presence(&mut self, state_key: String, path: String) {
        if !self.presence.iter().any(|e| e.state_key == state_key && e.path == path) {
            self.presence.push(PresenceEntry { state_key, path });
        }
    }
}
