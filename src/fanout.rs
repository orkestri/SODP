use std::sync::Arc;

use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::delta::DeltaOp;
use crate::frame::OutboundMsg;

/// Default capacity for the per-session bounded channel.
/// When the channel is full, the subscriber is considered slow and evicted.
pub const DEFAULT_BACKPRESSURE_LIMIT: usize = 1024;

/// One active WATCH subscription: a channel back to the owning connection task.
#[derive(Debug, Clone)]
pub struct Subscriber {
    pub session_id: String,
    pub stream_id: u32,
    pub tx: mpsc::Sender<OutboundMsg>,
    /// Cancelled when the subscriber is too slow (channel full).
    pub cancel: CancellationToken,
}

/// Registry of all active subscriptions, keyed by state name.
///
/// Designed for high-concurrency fanout:
/// - reads (broadcast) hold per-shard read locks only
/// - writes (subscribe/unsubscribe) hold per-shard write locks briefly
pub struct FanoutBus {
    subscriptions: DashMap<String, Vec<Subscriber>>,
}

/// Encode a DELTA body (version + ops) as MessagePack bytes.
///
/// The result is the shared body for all subscriber DELTA frames.  Call this
/// once per mutation, then pass the bytes to `broadcast_encoded` and/or
/// `frame::delta_bytes` for direct same-session delivery.
pub fn encode_delta_body(version: u64, ops: &[DeltaOp]) -> Vec<u8> {
    #[derive(Serialize)]
    struct DeltaBody<'a> {
        version: u64,
        ops: &'a [DeltaOp],
    }
    rmp_serde::to_vec_named(&DeltaBody { version, ops }).unwrap_or_default()
}

impl FanoutBus {
    pub fn new() -> Arc<Self> {
        Arc::new(FanoutBus { subscriptions: DashMap::new() })
    }

    pub fn subscribe(&self, state_key: String, sub: Subscriber) {
        self.subscriptions.entry(state_key).or_default().push(sub);
    }

    pub fn unsubscribe(&self, state_key: &str, session_id: &str) {
        if let Some(mut subs) = self.subscriptions.get_mut(state_key) {
            subs.retain(|s| s.session_id != session_id);
        }
    }

    /// Remove every subscription belonging to a session (called on disconnect).
    pub fn remove_session(&self, session_id: &str) {
        for mut entry in self.subscriptions.iter_mut() {
            entry.value_mut().retain(|s| s.session_id != session_id);
        }
    }

    /// Broadcast a pre-encoded delta body to subscribers of `state_key`.
    ///
    /// Optimised for low P99:
    ///  1. Snapshot subscriber handles while holding the shard lock for the
    ///     shortest possible window (just pointer copies, no allocation).
    ///  2. Body is already encoded — per-subscriber cost is one small Vec
    ///     alloc + header write.
    ///
    /// `exclude_session`: if `Some(id)`, skip the subscriber whose
    /// `session_id == id`.  Use this when the triggering session will receive
    /// its own DELTA via a direct `ws_tx.send()` call.
    ///
    /// Slow subscribers (full channel) are cancelled and removed inline.
    pub fn broadcast_encoded(
        &self,
        state_key: &str,
        body_mp: &[u8],
        exclude_session: Option<&str>,
    ) {
        let snapshot: Vec<(u32, String, mpsc::Sender<OutboundMsg>, CancellationToken)> =
            match self.subscriptions.get(state_key) {
                Some(subs) => subs
                    .iter()
                    .filter(|s| exclude_session.is_none_or(|ex| s.session_id != ex))
                    .map(|s| (s.stream_id, s.session_id.clone(), s.tx.clone(), s.cancel.clone()))
                    .collect(),
                None => return,
            };
        // DashMap shard lock released here.

        if snapshot.is_empty() { return; }

        // Wrap body_mp in Arc once — all subscribers share the bytes; only the
        // tiny per-subscriber header (stream_id) is assembled in the write task.
        let shared: Arc<[u8]> = Arc::from(body_mp);

        let mut slow_sessions: Vec<String> = Vec::new();

        for (stream_id, session_id, tx, cancel) in snapshot {
            if tx.try_send(OutboundMsg::ArcDelta { stream_id, body_mp: Arc::clone(&shared) }).is_err() {
                warn!("Slow consumer {session_id} — cancelling session");
                cancel.cancel();
                slow_sessions.push(session_id);
            }
        }

        // Remove slow subscribers from this key's subscription list.
        if !slow_sessions.is_empty()
            && let Some(mut subs) = self.subscriptions.get_mut(state_key) {
                subs.retain(|s| !slow_sessions.contains(&s.session_id));
        }
    }

    /// Broadcast delta ops to every subscriber of `state_key`.
    ///
    /// Encodes the body internally; use `encode_delta_body` + `broadcast_encoded`
    /// directly when you also need the bytes for a same-session direct write.
    pub fn broadcast(&self, state_key: &str, version: u64, ops: &[DeltaOp]) {
        if ops.is_empty() { return; }
        let body_mp = encode_delta_body(version, ops);
        if body_mp.is_empty() { return; }
        self.broadcast_encoded(state_key, &body_mp, None);
    }

    pub fn subscriber_count(&self, state_key: &str) -> usize {
        self.subscriptions.get(state_key).map(|s| s.len()).unwrap_or(0)
    }
}
