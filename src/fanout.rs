use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc::UnboundedSender;
use tracing::error;

use crate::delta::DeltaOp;
use crate::frame::{self, OutboundMsg};

/// One active WATCH subscription: a channel back to the owning connection task.
#[derive(Debug, Clone)]
pub struct Subscriber {
    pub session_id: String,
    pub stream_id: u32,
    pub tx: UnboundedSender<OutboundMsg>,
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
    let ops_value = match serde_json::to_value(ops) {
        Ok(v) => v,
        Err(e) => { error!("Failed to serialize delta ops: {e}"); return vec![]; }
    };
    rmp_serde::to_vec(&serde_json::json!({
        "version": version,
        "ops":     ops_value,
    })).unwrap_or_default()
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
    pub fn broadcast_encoded(
        &self,
        state_key: &str,
        body_mp: &[u8],
        exclude_session: Option<&str>,
    ) {
        let snapshot: Vec<(u32, UnboundedSender<OutboundMsg>)> =
            match self.subscriptions.get(state_key) {
                Some(subs) => subs
                    .iter()
                    .filter(|s| exclude_session.is_none_or(|ex| s.session_id != ex))
                    .map(|s| (s.stream_id, s.tx.clone()))
                    .collect(),
                None => return,
            };
        // DashMap shard lock released here.

        if snapshot.is_empty() { return; }

        for (stream_id, tx) in snapshot {
            let wire = frame::delta_bytes(stream_id, 0, body_mp);
            let _ = tx.send(OutboundMsg::Bytes(wire));
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
