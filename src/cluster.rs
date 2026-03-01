/// Redis-backed horizontal scaling for SODP.
///
/// When `SODP_REDIS_URL` is set, the server:
///   1. On startup — loads all persisted state from Redis into the local `StateStore`.
///   2. On every mutation — fires a background task that writes the new value to Redis
///      (`HSET sodp:state`) and publishes an encoded delta to all peer nodes
///      (`PUBLISH sodp:delta:{key}`).
///   3. Runs a background subscriber that listens for deltas from other nodes and
///      delivers them to local watchers via `FanoutBus`.
///
/// Design constraints:
///   - Sync is fire-and-forget (no latency added to the mutation hot path).
///   - `MultiplexedConnection::clone()` allows concurrent async commands without a `Mutex`.
///   - The Pub/Sub subscriber requires a dedicated connection (Redis protocol restriction).
///   - Cross-node RESUME falls back to STATE_INIT — same graceful path as local eviction.

use std::sync::Arc;
use std::time::Duration;

use redis::AsyncCommands;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::fanout::FanoutBus;

/// Shared Redis state for cross-node synchronization.
pub struct RedisCluster {
    /// Unique identifier for this server node.  Used to skip own messages in
    /// the Pub/Sub subscriber — we already delivered them locally.
    pub node_id: String,
    /// Redis client used to open new connections (e.g. a dedicated Pub/Sub
    /// connection).
    client: redis::Client,
    /// Multiplexed async connection shared by concurrent command senders.
    /// `MultiplexedConnection` is `Clone` — each clone shares the same
    /// underlying pipeline without any extra mutex overhead.
    conn: redis::aio::MultiplexedConnection,
}

impl RedisCluster {
    /// Connect to Redis and return an `Arc<RedisCluster>`.
    ///
    /// Fails fast if the URL is invalid or the server is unreachable at startup.
    pub async fn connect(url: &str) -> anyhow::Result<Arc<Self>> {
        let client = redis::Client::open(url)?;
        let conn   = client.get_multiplexed_async_connection().await?;
        let node_id = uuid::Uuid::new_v4().to_string();
        info!("Redis cluster connected — node_id={node_id}");
        Ok(Arc::new(RedisCluster { node_id, client, conn }))
    }

    /// Load all state entries from Redis at startup.
    ///
    /// Returns a list of `(key, version, value)` tuples.  Caller passes these
    /// to `StateStore::load_entries()` so that Redis wins over any stale local
    /// disk state when a node rejoins a cluster.
    pub async fn load_state(&self) -> anyhow::Result<Vec<(String, u64, serde_json::Value)>> {
        let mut conn = self.conn.clone();
        let raw: Vec<(Vec<u8>, Vec<u8>)> = conn.hgetall("sodp:state").await?;

        let mut out = Vec::with_capacity(raw.len());
        for (k_bytes, v_bytes) in raw {
            let key = match String::from_utf8(k_bytes) {
                Ok(k) => k,
                Err(e) => { warn!("Redis: non-UTF-8 state key: {e}"); continue; }
            };
            match rmp_serde::from_slice::<(u64, serde_json::Value)>(&v_bytes) {
                Ok((version, value)) => out.push((key, version, value)),
                Err(e) => warn!("Redis: failed to decode state for key '{key}': {e}"),
            }
        }

        info!("Redis: loaded {} state entries", out.len());
        Ok(out)
    }

    /// Fire-and-forget: persist `(version, value)` to Redis and publish the
    /// encoded delta body to all peer nodes.
    ///
    /// Spawns a background task — the mutation hot path is not blocked.
    pub fn sync(
        self:    &Arc<Self>,
        key:     String,
        version: u64,
        value:   serde_json::Value,
        body_mp: Vec<u8>,
    ) {
        let mut conn    = self.conn.clone();
        let node_id     = self.node_id.clone();
        let channel     = format!("sodp:delta:{key}");

        tokio::spawn(async move {
            // Encode (version, value) as MessagePack for compact storage.
            let encoded = match rmp_serde::to_vec_named(&(version, &value)) {
                Ok(b)  => b,
                Err(e) => { error!("Redis sync: encode error for '{key}': {e}"); return; }
            };

            if let Err(e) = conn.hset::<_, _, _, ()>("sodp:state", &key, &encoded).await {
                error!("Redis sync: HSET failed for '{key}': {e}");
            }

            // Publish: [node_id_bytes, body_mp_bytes] — msgpack tuple
            let msg = match rmp_serde::to_vec_named(&(node_id.as_str(), body_mp.as_slice())) {
                Ok(b)  => b,
                Err(e) => { error!("Redis sync: encode pub msg for '{key}': {e}"); return; }
            };
            if let Err(e) = conn.publish::<_, _, ()>(&channel, &msg).await {
                error!("Redis sync: PUBLISH failed for '{key}': {e}");
            } else {
                debug!("Redis sync: published delta for '{key}' v{version}");
            }
        });
    }

    /// Fire-and-forget: remove `key` from Redis state and publish a delete delta.
    pub fn sync_delete(
        self:    &Arc<Self>,
        key:     String,
        body_mp: Vec<u8>,
    ) {
        let mut conn = self.conn.clone();
        let node_id  = self.node_id.clone();
        let channel  = format!("sodp:delta:{key}");

        tokio::spawn(async move {
            if let Err(e) = conn.hdel::<_, _, ()>("sodp:state", &key).await {
                error!("Redis sync_delete: HDEL failed for '{key}': {e}");
            }

            let msg = match rmp_serde::to_vec_named(&(node_id.as_str(), body_mp.as_slice())) {
                Ok(b)  => b,
                Err(e) => { error!("Redis sync_delete: encode pub msg for '{key}': {e}"); return; }
            };
            if let Err(e) = conn.publish::<_, _, ()>(&channel, &msg).await {
                error!("Redis sync_delete: PUBLISH failed for '{key}': {e}");
            }
        });
    }

    /// Spawn a long-running background task that subscribes to all SODP delta
    /// channels and delivers incoming deltas from peer nodes to local watchers.
    ///
    /// Reconnects automatically on error (2-second delay).  Stops cleanly when
    /// `shutdown` is cancelled.
    pub fn spawn_subscriber(
        self:     Arc<Self>,
        fanout:   Arc<FanoutBus>,
        shutdown: CancellationToken,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        info!("Redis subscriber: shutting down");
                        break;
                    }
                    _ = self.run_subscriber_loop(&fanout, &shutdown) => {
                        // run_subscriber_loop only returns on error — reconnect.
                        if shutdown.is_cancelled() { break; }
                        warn!("Redis subscriber: disconnected, reconnecting in 2s...");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        });
    }

    /// Inner Pub/Sub loop.  Opens a dedicated connection, subscribes to the
    /// pattern, and forwards messages until the connection drops or shutdown fires.
    async fn run_subscriber_loop(&self, fanout: &Arc<FanoutBus>, shutdown: &CancellationToken) {
        let mut pubsub = match self.client.get_async_pubsub().await {
            Ok(c)  => c,
            Err(e) => { error!("Redis: failed to open pubsub connection: {e}"); return; }
        };

        if let Err(e) = pubsub.psubscribe("sodp:delta:*").await {
            error!("Redis: PSUBSCRIBE failed: {e}");
            return;
        }
        info!("Redis subscriber: listening on sodp:delta:*");

        use futures_util::StreamExt;
        let mut stream = pubsub.into_on_message();

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                msg = stream.next() => {
                    let msg = match msg {
                        Some(m) => m,
                        None    => { warn!("Redis subscriber: stream ended"); break; }
                    };
                    self.handle_pubsub_message(&msg, fanout);
                }
            }
        }
    }

    /// Decode a Pub/Sub message and deliver it to local watchers.
    fn handle_pubsub_message(&self, msg: &redis::Msg, fanout: &FanoutBus) {
        // Channel name: "sodp:delta:{key}"
        let channel: String = match msg.get_channel() {
            Ok(c)  => c,
            Err(e) => { warn!("Redis subscriber: bad channel: {e}"); return; }
        };
        let key = channel.strip_prefix("sodp:delta:").unwrap_or(&channel);

        let payload: Vec<u8> = match msg.get_payload() {
            Ok(p)  => p,
            Err(e) => { warn!("Redis subscriber: bad payload: {e}"); return; }
        };

        // Decode (sender_node_id, body_mp)
        let (sender_id, body_mp): (String, Vec<u8>) = match rmp_serde::from_slice(&payload) {
            Ok(v)  => v,
            Err(e) => { warn!("Redis subscriber: decode failed for '{key}': {e}"); return; }
        };

        // Skip messages originating from this node — already delivered locally.
        if sender_id == self.node_id {
            return;
        }

        debug!("Redis subscriber: delta from node {sender_id} for '{key}'");
        fanout.broadcast_encoded(key, &body_mp, None);
    }
}
