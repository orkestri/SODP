use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};
use tracing::{debug, error, info, warn};

use crate::acl::AclRegistry;
use crate::cluster::RedisCluster;
use crate::fanout::{encode_delta_body, FanoutBus, Subscriber};
use crate::frame::{self, types, Frame, OutboundMsg};
use crate::schema::SchemaRegistry;
use crate::session::Session;
use crate::state::StateStore;

/// Algorithm and key material used to validate incoming JWTs.
pub enum JwtConfig {
    /// HMAC-SHA256 — shared secret (simpler, suitable for single-host setups).
    Hs256 { secret: String },
    /// RSA-SHA256 — only the public key lives here; the private key stays in
    /// the backend that issues tokens.  Recommended for production deployments.
    Rs256 { public_key_pem: String },
}

/// Load ACL from the file pointed to by `SODP_ACL_FILE`.  Returns `None` when
/// the variable is unset (backward-compatible: all access allowed) or on error
/// (logged; server starts without ACL enforcement to avoid a hard failure).
fn read_acl() -> Option<Arc<AclRegistry>> {
    let path = std::env::var("SODP_ACL_FILE").ok()?;
    match AclRegistry::from_file(std::path::Path::new(&path)) {
        Ok(acl) => {
            tracing::info!("ACL loaded from {path}");
            Some(Arc::new(acl))
        }
        Err(e) => {
            tracing::error!("Failed to load ACL file '{path}': {e} — running without ACL");
            None
        }
    }
}

/// Read JWT configuration from environment variables.
/// Priority: RS256 (public key) takes precedence over HS256 (secret).
fn read_jwt_config() -> Option<JwtConfig> {
    // Inline PEM (newlines may be escaped as \n in env vars).
    if let Ok(pem) = std::env::var("SODP_JWT_PUBLIC_KEY") {
        return Some(JwtConfig::Rs256 { public_key_pem: pem.replace("\\n", "\n") });
    }
    // PEM loaded from a file path.
    if let Ok(path) = std::env::var("SODP_JWT_PUBLIC_KEY_FILE") {
        match std::fs::read_to_string(&path) {
            Ok(pem) => return Some(JwtConfig::Rs256 { public_key_pem: pem }),
            Err(e)  => error!("Failed to read SODP_JWT_PUBLIC_KEY_FILE '{path}': {e}"),
        }
    }
    // Fallback: shared HS256 secret.
    if let Ok(secret) = std::env::var("SODP_JWT_SECRET") {
        return Some(JwtConfig::Hs256 { secret });
    }
    None
}

/// Duration until the Unix `exp` timestamp.  Returns `ZERO` if already past.
fn exp_remaining(exp: usize) -> Duration {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let exp_secs = exp as u64;
    if exp_secs > now { Duration::from_secs(exp_secs - now) } else { Duration::ZERO }
}

/// JWT claims decoded from a client AUTH frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub struct SodpServer {
    pub state:            Arc<StateStore>,
    pub fanout:           Arc<FanoutBus>,
    pub schema:           Option<Arc<SchemaRegistry>>,
    /// Per-key ACL.  `None` = ACL disabled (all access allowed).
    pub acl:              Option<Arc<AclRegistry>>,
    /// JWT validation config.  `None` = auth disabled.
    pub jwt_config:       Option<JwtConfig>,
    /// Live connection counter (incremented on accept, decremented on exit).
    pub connections:      Arc<AtomicUsize>,
    /// Maximum concurrent connections (`SODP_MAX_CONNECTIONS`).  `None` = unlimited.
    pub max_connections:  Option<usize>,
    /// Maximum inbound frame size in bytes (`SODP_MAX_FRAME_BYTES`).  Default: 1 MiB.
    pub max_frame_bytes:  usize,
    /// Seconds between outbound WebSocket pings (`SODP_WS_PING_INTERVAL`).  0 = disabled.
    pub ws_ping_interval: u64,
    /// Max CALL frames per second per session (`SODP_RATE_WRITES_PER_SEC`).  `None` = unlimited.
    pub rate_writes_per_sec: Option<u32>,
    /// Max WATCH/RESUME frames per second per session (`SODP_RATE_WATCHES_PER_SEC`).  `None` = unlimited.
    pub rate_watches_per_sec: Option<u32>,
    /// Redis cluster for cross-node state sync and fanout.  `None` = single-node mode.
    pub cluster: Option<Arc<RedisCluster>>,
}

/// Read connection / frame-size / ping / rate limits from environment variables.
fn read_limits() -> (Option<usize>, usize, u64, Option<u32>, Option<u32>) {
    let max_connections = std::env::var("SODP_MAX_CONNECTIONS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok());
    let max_frame_bytes = std::env::var("SODP_MAX_FRAME_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1_048_576); // 1 MiB default
    // SODP_WS_PING_INTERVAL — seconds between outbound WS pings (0 = disabled, default 25).
    let ws_ping_interval = std::env::var("SODP_WS_PING_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(25);
    // SODP_RATE_WRITES_PER_SEC  — max CALL frames/s per session (unset = unlimited).
    let rate_writes = std::env::var("SODP_RATE_WRITES_PER_SEC")
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    // SODP_RATE_WATCHES_PER_SEC — max WATCH/RESUME frames/s per session (unset = unlimited).
    let rate_watches = std::env::var("SODP_RATE_WATCHES_PER_SEC")
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    (max_connections, max_frame_bytes, ws_ping_interval, rate_writes, rate_watches)
}

impl SodpServer {
    pub fn new() -> Arc<Self> {
        let (max_connections, max_frame_bytes, ws_ping_interval, rate_writes_per_sec, rate_watches_per_sec) = read_limits();
        Arc::new(SodpServer {
            state:                StateStore::new(),
            fanout:               FanoutBus::new(),
            schema:               None,
            acl:                  read_acl(),
            jwt_config:           read_jwt_config(),
            connections:          Arc::new(AtomicUsize::new(0)),
            max_connections,
            max_frame_bytes,
            ws_ping_interval,
            rate_writes_per_sec,
            rate_watches_per_sec,
            cluster:              None,
        })
    }

    pub fn new_persistent(log_dir: &std::path::Path) -> anyhow::Result<Arc<Self>> {
        let (max_connections, max_frame_bytes, ws_ping_interval, rate_writes_per_sec, rate_watches_per_sec) = read_limits();
        Ok(Arc::new(SodpServer {
            state:                StateStore::open(log_dir)?,
            fanout:               FanoutBus::new(),
            schema:               None,
            acl:                  read_acl(),
            jwt_config:           read_jwt_config(),
            connections:          Arc::new(AtomicUsize::new(0)),
            max_connections,
            max_frame_bytes,
            ws_ping_interval,
            rate_writes_per_sec,
            rate_watches_per_sec,
            cluster:              None,
        }))
    }

    /// Create a server with an optional on-disk log and a JSON schema file.
    pub fn with_schema(
        log_dir:     Option<&std::path::Path>,
        schema_path: &std::path::Path,
    ) -> anyhow::Result<Arc<Self>> {
        let (max_connections, max_frame_bytes, ws_ping_interval, rate_writes_per_sec, rate_watches_per_sec) = read_limits();
        let schema = SchemaRegistry::from_file(schema_path)?;
        let state  = match log_dir {
            Some(dir) => StateStore::open(dir)?,
            None      => StateStore::new(),
        };
        Ok(Arc::new(SodpServer {
            state,
            fanout:               FanoutBus::new(),
            schema:               Some(Arc::new(schema)),
            acl:                  read_acl(),
            jwt_config:           read_jwt_config(),
            connections:          Arc::new(AtomicUsize::new(0)),
            max_connections,
            max_frame_bytes,
            ws_ping_interval,
            rate_writes_per_sec,
            rate_watches_per_sec,
            cluster:              None,
        }))
    }

    pub async fn listen(self: Arc<Self>, addr: &str, shutdown: CancellationToken) -> anyhow::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        info!("SODP server listening on {addr}");

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, peer)) => {
                            if let Some(limit) = self.max_connections {
                                if self.connections.load(Ordering::Relaxed) >= limit {
                                    warn!("Max connections ({limit}) reached — rejecting {peer}");
                                    continue; // drop(stream) is implicit
                                }
                            }
                            info!("New connection from {peer}");
                            let n = self.connections.fetch_add(1, Ordering::Relaxed) + 1;
                            metrics::gauge!("sodp_connections_active").set(n as f64);
                            let server = Arc::clone(&self);
                            let conns  = Arc::clone(&self.connections);
                            let token  = shutdown.clone();
                            tokio::spawn(async move {
                                if let Err(e) = server.handle_connection(stream, token).await {
                                    warn!("Connection closed with error: {e}");
                                }
                                let n = conns.fetch_sub(1, Ordering::Relaxed) - 1;
                                metrics::gauge!("sodp_connections_active").set(n as f64);
                            });
                        }
                        Err(e) => error!("Accept error: {e}"),
                    }
                }
                _ = shutdown.cancelled() => {
                    info!("Shutting down");
                    break;
                }
            }
        }
        Ok(())
    }

    async fn handle_connection(self: Arc<Self>, stream: TcpStream, shutdown: CancellationToken) -> anyhow::Result<()> {
        // Disable Nagle's algorithm so small frames (e.g. 54-byte DELTAs) are
        // transmitted immediately rather than waiting for a full-MSS coalesce.
        stream.set_nodelay(true)?;

        let ws = accept_async(stream).await?;
        let (mut ws_tx, mut ws_rx) = ws.split();

        let mut session = Session::new(self.rate_writes_per_sec, self.rate_watches_per_sec);
        let session_id = session.id.clone();

        // Channel used ONLY for fanout DELTAs delivered from OTHER sessions.
        // Responses to the current session's own requests (STATE_INIT, DELTA for
        // own-watched keys, RESULT, errors) are written directly to ws_tx —
        // eliminating the channel hop and the extra select! loop iteration.
        let (tx, mut rx) = mpsc::unbounded_channel::<OutboundMsg>();

        let auth_required = self.jwt_config.is_some();
        ws_tx.send(Message::Binary(frame::hello(auth_required).encode()?.into())).await?;

        // If auth is enabled, complete the JWT handshake before the event loop.
        let opt_claims: Option<Claims> = if let Some(config) = &self.jwt_config {
            match self.auth_handshake(&mut ws_rx, &mut ws_tx, config).await {
                Ok(claims) => {
                    info!("Session {session_id} authenticated as '{}'", claims.sub);
                    session.sub    = Some(claims.sub.clone());
                    session.claims = serde_json::Value::Object(claims.extra.clone());
                    Some(claims)
                }
                Err(e) => {
                    warn!("Session {session_id} auth failed: {e}");
                    return Ok(());
                }
            }
        } else {
            None
        };

        info!("Session {session_id} started");

        // Schedule connection close when the JWT's `exp` is reached.
        // When auth is disabled we use a very long sentinel (1 year) so the
        // select! arm is effectively dead but requires no special-casing.
        let token_ttl = opt_claims
            .as_ref()
            .map(|c| exp_remaining(c.exp))
            .unwrap_or(Duration::from_secs(365 * 24 * 3600));
        let token_expiry = tokio::time::sleep(token_ttl);
        tokio::pin!(token_expiry);

        // WebSocket protocol-level ping/pong for zombie-connection detection.
        // Uses `interval_at` so the first tick fires after one full period —
        // no immediate ping on connect.  When disabled, the period is 1 year
        // so the arm never fires in practice.
        let ping_period = if self.ws_ping_interval == 0 {
            Duration::from_secs(365 * 24 * 3600) // disabled sentinel
        } else {
            Duration::from_secs(self.ws_ping_interval)
        };
        let mut ping_ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + ping_period,
            ping_period,
        );
        let mut awaiting_pong = false;

        // Single-task event loop.
        //
        // No `biased` — equal polling priority prevents starvation when the
        // outbound channel is flooded with fanout DELTAs.  Since own-session
        // responses bypass the channel entirely, the channel is only active for
        // fanout from other sessions, so starvation is not a concern in practice.
        loop {
            tokio::select! {
                // ── outbound: fanout DELTAs from other sessions ───────────────
                msg = rx.recv() => {
                    let Some(outbound) = msg else { break };
                    let result = match outbound {
                        OutboundMsg::Frame(f) => match f.encode() {
                            Ok(bytes) => ws_tx.send(Message::Binary(bytes.into())).await,
                            Err(e) => { warn!("Frame encode error: {e}"); continue; }
                        },
                        OutboundMsg::Bytes(b) => ws_tx.send(Message::Binary(b.into())).await,
                    };
                    if result.is_err() { break; }
                }

                // ── inbound: new WebSocket frame from the client ──────────────
                msg = ws_rx.next() => {
                    match msg {
                        Some(Ok(Message::Binary(bytes))) => {
                            if bytes.len() > self.max_frame_bytes {
                                warn!("[{session_id}] Frame too large ({} B > {} B limit) — closing", bytes.len(), self.max_frame_bytes);
                                let _ = ws_tx.send(Message::Binary(
                                    frame::error(0, 0, 413, "frame too large").encode()?.into()
                                )).await;
                                break;
                            }
                            match Frame::decode(&bytes) {
                                Ok(frame) => {
                                    if self.handle_frame(frame, &mut session, &mut ws_tx, &tx).await.is_err() {
                                        break;
                                    }
                                }
                                Err(e) => warn!("[{session_id}] Decode error: {e}"),
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            // Respond with WS Pong (protocol requirement).
                            if ws_tx.send(Message::Pong(data)).await.is_err() { break; }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            // Client responded to our ping — connection is alive.
                            awaiting_pong = false;
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        _ => {}
                    }
                }

                _ = shutdown.cancelled() => {
                    debug!("Session {session_id} shutting down");
                    break;
                }

                _ = &mut token_expiry => {
                    info!("Session {session_id} token expired — closing");
                    let _ = ws_tx.send(Message::Binary(
                        frame::error(0, 0, 401, "token expired").encode()?.into()
                    )).await;
                    break;
                }

                // ── WS ping / zombie detection
                _ = ping_ticker.tick() => {
                    if awaiting_pong {
                        // Previous ping was never answered — zombie connection.
                        info!("Session {session_id} ping timeout — closing");
                        break;
                    }
                    awaiting_pong = true;
                    if ws_tx.send(Message::Ping(vec![])).await.is_err() { break; }
                }
            }
        }

        // ── Presence cleanup ──────────────────────────────────────────────────
        // For every session-owned path, remove it from state and broadcast the
        // DELTA so all remaining watchers see the user disappear immediately.
        for entry in &session.presence {
            let current = match self.state.get(&entry.state_key) {
                Some(e) => e.value,
                None    => continue,
            };
            let updated = json_remove_in(current, &entry.path);
            let (version, ops) = self.state.apply(&entry.state_key, updated.clone());
            if !ops.is_empty() {
                let body_mp = encode_delta_body(version, &ops);
                broadcast_timed(&self.fanout, &entry.state_key, &body_mp, None);
                if let Some(cluster) = &self.cluster {
                    cluster.sync(entry.state_key.clone(), version, updated, body_mp);
                }
            }
        }

        self.fanout.remove_session(&session_id);
        info!("Session {session_id} ended");
        Ok(())
    }

    // --- AUTH handshake ---

    /// Run the JWT auth handshake: wait for an AUTH frame, validate the token,
    /// send AUTH_OK on success or ERROR 401 on failure.
    ///
    /// HEARTBEAT frames are allowed during the handshake (keep-alive from slow
    /// clients).  Any other frame type before AUTH is rejected with 401.
    async fn auth_handshake(
        &self,
        ws_rx:  &mut SplitStream<WebSocketStream<TcpStream>>,
        ws_tx:  &mut SplitSink<WebSocketStream<TcpStream>, Message>,
        config: &JwtConfig,
    ) -> anyhow::Result<Claims> {
        loop {
            match ws_rx.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    if bytes.len() > self.max_frame_bytes {
                        let _ = ws_tx.send(Message::Binary(
                            frame::error(0, 0, 413, "frame too large").encode()?.into()
                        )).await;
                        return Err(anyhow::anyhow!("frame too large during auth ({} B)", bytes.len()));
                    }
                    let frame = match Frame::decode(&bytes) {
                        Ok(f)  => f,
                        Err(e) => {
                            let _ = ws_tx.send(Message::Binary(
                                frame::error(0, 0, 401, "bad frame during auth").encode()?.into()
                            )).await;
                            return Err(e.into());
                        }
                    };

                    match frame.frame_type {
                        types::HEARTBEAT => {
                            // Allow heartbeats during auth; echo one back.
                            let _ = ws_tx.send(Message::Binary(frame::heartbeat().encode()?.into())).await;
                        }

                        types::AUTH => {
                            let token = frame.body
                                .get("token")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            match Self::validate_jwt(token, config) {
                                Ok(claims) => {
                                    ws_tx.send(Message::Binary(
                                        frame::auth_ok(&claims.sub).encode()?.into()
                                    )).await?;
                                    return Ok(claims);
                                }
                                Err(e) => {
                                    let msg = format!("auth failed: {e}");
                                    let _ = ws_tx.send(Message::Binary(
                                        frame::error(0, 0, 401, &msg).encode()?.into()
                                    )).await;
                                    return Err(anyhow::anyhow!(msg));
                                }
                            }
                        }

                        _ => {
                            let _ = ws_tx.send(Message::Binary(
                                frame::error(0, 0, 401, "auth required").encode()?.into()
                            )).await;
                            return Err(anyhow::anyhow!("auth required — unexpected frame before AUTH"));
                        }
                    }
                }
                Some(Ok(Message::Ping(_))) => {
                    let _ = ws_tx.send(Message::Binary(frame::heartbeat().encode()?.into())).await;
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    return Err(anyhow::anyhow!("connection closed before auth"));
                }
                _ => {}
            }
        }
    }

    fn validate_jwt(token: &str, config: &JwtConfig) -> anyhow::Result<Claims> {
        let (key, algo) = match config {
            JwtConfig::Hs256 { secret } => (
                DecodingKey::from_secret(secret.as_bytes()),
                Algorithm::HS256,
            ),
            JwtConfig::Rs256 { public_key_pem } => (
                DecodingKey::from_rsa_pem(public_key_pem.as_bytes())
                    .map_err(|e| anyhow::anyhow!("RS256 key error: {e}"))?,
                Algorithm::RS256,
            ),
        };
        let mut validation = Validation::new(algo);
        validation.leeway = 0;  // no clock-skew tolerance — tokens must not be expired
        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| anyhow::anyhow!("JWT error: {e}"))?;
        Ok(data.claims)
    }

    async fn handle_frame(
        &self,
        frame: Frame,
        session: &mut Session,
        ws_tx: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
        tx: &mpsc::UnboundedSender<OutboundMsg>,
    ) -> anyhow::Result<()> {
        let type_label: &'static str = match frame.frame_type {
            types::WATCH     => "watch",
            types::CALL      => "call",
            types::RESUME    => "resume",
            types::UNWATCH   => "unwatch",
            types::ACK       => "ack",
            types::HEARTBEAT => "heartbeat",
            _                => "unknown",
        };
        metrics::counter!("sodp_frames_rx_total", "type" => type_label).increment(1);

        match frame.frame_type {
            types::WATCH     => self.handle_watch(frame, session, ws_tx, tx).await,
            types::RESUME    => self.handle_resume(frame, session, ws_tx, tx).await,
            types::UNWATCH   => self.handle_unwatch(frame, session),
            types::CALL      => self.handle_call(frame, session, ws_tx).await,
            types::ACK       => {
                session.ack_stream(frame.stream_id, {
                    frame.body.get("seq").and_then(|v| v.as_u64()).unwrap_or(0)
                });
                Ok(())
            }
            types::HEARTBEAT => {
                ws_tx.send(Message::Binary(frame::heartbeat().encode()?.into())).await?;
                Ok(())
            }
            t => { warn!("Unhandled frame type: {t:#04x}"); Ok(()) }
        }
    }

    // --- WATCH ---

    async fn handle_watch(
        &self,
        frame: Frame,
        session: &mut Session,
        ws_tx: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
        tx: &mpsc::UnboundedSender<OutboundMsg>,
    ) -> anyhow::Result<()> {
        if let Some(ref mut lim) = session.watch_limiter {
            if !lim.allow() {
                metrics::counter!("sodp_rate_limited_total", "type" => "watch").increment(1);
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(frame.stream_id, seq, 429, "rate limit exceeded").encode()?.into()
                )).await?;
                return Ok(());
            }
        }

        let state_key = match frame.body.get("state").and_then(|v| v.as_str()) {
            Some(k) => k.to_string(),
            None => {
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(frame.stream_id, seq, 400, "WATCH: missing 'state' key").encode()?.into()
                )).await?;
                return Ok(());
            }
        };

        if let Some(acl) = &self.acl {
            if !acl.can_read(&state_key, session.sub.as_deref(), &session.claims) {
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(frame.stream_id, seq, 403, "forbidden").encode()?.into()
                )).await?;
                return Ok(());
            }
        }

        let stream_id = session.allocate_stream();
        session.add_watch(stream_id, state_key.clone());

        // Register subscriber so future broadcasts reach this session.
        self.fanout.subscribe(
            state_key.clone(),
            Subscriber { session_id: session.id.clone(), stream_id, tx: tx.clone() },
        );

        // Send current snapshot.  `initialized: false` when the key has never
        // been written — lets clients distinguish "not yet set" from null.
        let (version, value, initialized) = match self.state.get(&state_key) {
            Some(e) => (e.version, e.value, true),
            None    => (0, serde_json::Value::Null, false),
        };

        let seq = session.next_seq();
        ws_tx.send(Message::Binary(
            frame::state_init(stream_id, seq, &state_key, version, value, initialized).encode()?.into()
        )).await?;

        debug!("Session {} watching '{}' on stream {}", session.id, state_key, stream_id);
        Ok(())
    }


    // --- UNWATCH ---

    fn handle_unwatch(&self, frame: Frame, session: &mut Session) -> anyhow::Result<()> {
        let state_key = match frame.body.get("state").and_then(|v| v.as_str()) {
            Some(k) => k,
            None    => return Ok(()), // malformed frame — ignore silently
        };

        if let Some(stream_id) = session.watch_stream_id(state_key) {
            session.remove_watch(stream_id);
            self.fanout.unsubscribe(state_key, &session.id);
            debug!("Session {} unwatched '{}'", session.id, state_key);
        }
        Ok(())
    }

    // --- RESUME ---

    async fn handle_resume(
        &self,
        frame: Frame,
        session: &mut Session,
        ws_tx: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
        tx: &mpsc::UnboundedSender<OutboundMsg>,
    ) -> anyhow::Result<()> {
        if let Some(ref mut lim) = session.watch_limiter {
            if !lim.allow() {
                metrics::counter!("sodp_rate_limited_total", "type" => "resume").increment(1);
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(frame.stream_id, seq, 429, "rate limit exceeded").encode()?.into()
                )).await?;
                return Ok(());
            }
        }

        let state_key = match frame.body.get("state").and_then(|v| v.as_str()) {
            Some(k) => k.to_string(),
            None => {
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(frame.stream_id, seq, 400, "RESUME: missing 'state' key").encode()?.into()
                )).await?;
                return Ok(());
            }
        };

        if let Some(acl) = &self.acl {
            if !acl.can_read(&state_key, session.sub.as_deref(), &session.claims) {
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(frame.stream_id, seq, 403, "forbidden").encode()?.into()
                )).await?;
                return Ok(());
            }
        }

        let since_version = frame.body
            .get("since_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        // Allocate stream and subscribe to live updates — same as WATCH.
        let stream_id = session.allocate_stream();
        session.add_watch(stream_id, state_key.clone());
        self.fanout.subscribe(
            state_key.clone(),
            Subscriber { session_id: session.id.clone(), stream_id, tx: tx.clone() },
        );

        // Replay every delta the client missed.
        if since_version > 0 {
            let missed = self.state.deltas_since(&state_key, since_version);
            for (version, ops) in &missed {
                let seq = session.next_seq();
                let body_mp = encode_delta_body(*version, ops);
                let wire = frame::delta_bytes(stream_id, seq, &body_mp);
                ws_tx.send(Message::Binary(wire.into())).await?;
            }
        }

        // Conclude with STATE_INIT — consistent "you are now live" marker,
        // identical to what WATCH sends.  The client can verify their
        // reconstructed state matches the authoritative snapshot.
        let (version, value, initialized) = match self.state.get(&state_key) {
            Some(e) => (e.version, e.value, true),
            None    => (0, serde_json::Value::Null, false),
        };
        let seq = session.next_seq();
        ws_tx.send(Message::Binary(
            frame::state_init(stream_id, seq, &state_key, version, value, initialized).encode()?.into()
        )).await?;

        debug!(
            "Session {} resumed '{}' on stream {} since v{}",
            session.id, state_key, stream_id, since_version
        );
        Ok(())
    }

    // --- CALL ---

    async fn handle_call(
        &self,
        frame: Frame,
        session: &mut Session,
        ws_tx: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
    ) -> anyhow::Result<()> {
        let call_id = frame
            .body
            .get("call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let method = match frame.body.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => {
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(frame.stream_id, seq, 400, "CALL: missing 'method'").encode()?.into()
                )).await?;
                return Ok(());
            }
        };

        let args = frame.body.get("args").cloned().unwrap_or(serde_json::Value::Null);

        if let Some(ref mut lim) = session.write_limiter {
            if !lim.allow() {
                metrics::counter!("sodp_rate_limited_total", "type" => "write").increment(1);
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(frame.stream_id, seq, 429, "rate limit exceeded").encode()?.into()
                )).await?;
                return Ok(());
            }
        }

        // ACL write check — all CALL methods carry the target key in args.state.
        // An absent key will be caught by the individual method arms (→ 400).
        if let Some(acl) = &self.acl {
            if let Some(state_key) = args.get("state").and_then(|v| v.as_str()) {
                if !acl.can_write(state_key, session.sub.as_deref(), &session.claims) {
                    let seq = session.next_seq();
                    ws_tx.send(Message::Binary(
                        frame::error(frame.stream_id, seq, 403, "forbidden").encode()?.into()
                    )).await?;
                    return Ok(());
                }
            }
        }

        let method_label: &'static str = match method.as_str() {
            "state.set"      => "state.set",
            "state.patch"    => "state.patch",
            "state.set_in"   => "state.set_in",
            "state.delete"   => "state.delete",
            "state.presence" => "state.presence",
            _                => "unknown",
        };
        metrics::counter!("sodp_calls_total", "method" => method_label).increment(1);

        match method.as_str() {
            // state.set — replace entire state value
            // args: { state: "<key>", value: <any> }
            "state.set" => {
                let (state_key, new_value) = match extract_state_and_value(&args, "state.set") {
                    Ok(v) => v,
                    Err(msg) => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, &msg).encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };


                // Validate against schema before applying.
                if let Some(schema) = &self.schema {
                    if let Err(msg) = schema.validate(&state_key, &new_value) {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 422, &msg).encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                }

                let (version, ops) = self.state.apply(&state_key, new_value.clone());
                metrics::gauge!("sodp_state_keys").set(self.state.key_count() as f64);
                if !ops.is_empty() {
                    // Encode body once for both broadcast and direct write.
                    let body_mp = encode_delta_body(version, &ops);

                    // Fan out to every OTHER session watching this key.
                    broadcast_timed(&self.fanout, &state_key, &body_mp, Some(&session.id));

                    // If this session is watching the same key, deliver directly —
                    // no channel hop, no extra select! iteration.
                    if let Some(stream_id) = session.watch_stream_id(&state_key) {
                        let wire = frame::delta_bytes(stream_id, 0, &body_mp);
                        ws_tx.send(Message::Binary(wire.into())).await?;
                    }

                    // Cross-node sync (fire-and-forget).
                    if let Some(cluster) = &self.cluster {
                        cluster.sync(state_key.clone(), version, new_value, body_mp);
                    }
                }

                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::result_ok(
                        frame.stream_id,
                        seq,
                        &call_id,
                        Some(serde_json::json!({ "version": version })),
                    ).encode()?.into()
                )).await?;
            }

            // state.patch — shallow-merge a partial object into existing state
            // args: { state: "<key>", patch: { <field>: <value>, ... } }
            "state.patch" => {
                let state_key = match args.get("state").and_then(|v| v.as_str()) {
                    Some(k) => k.to_string(),
                    None => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, "state.patch: missing 'state' key").encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };

                let patch = match args.get("patch").cloned() {
                    Some(p) if p.is_object() => p,
                    _ => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, "state.patch: 'patch' must be an object").encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };


                let current = self.state
                    .get(&state_key)
                    .map(|e| e.value)
                    .unwrap_or(serde_json::json!({}));

                let merged = json_merge(current, patch);

                // Validate the merged value against schema before applying.
                if let Some(schema) = &self.schema {
                    if let Err(msg) = schema.validate(&state_key, &merged) {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 422, &msg).encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                }

                let (version, ops) = self.state.apply(&state_key, merged.clone());
                metrics::gauge!("sodp_state_keys").set(self.state.key_count() as f64);

                if !ops.is_empty() {
                    let body_mp = encode_delta_body(version, &ops);
                    broadcast_timed(&self.fanout, &state_key, &body_mp, Some(&session.id));

                    if let Some(stream_id) = session.watch_stream_id(&state_key) {
                        let wire = frame::delta_bytes(stream_id, 0, &body_mp);
                        ws_tx.send(Message::Binary(wire.into())).await?;
                    }

                    if let Some(cluster) = &self.cluster {
                        cluster.sync(state_key.clone(), version, merged, body_mp);
                    }
                }

                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::result_ok(
                        frame.stream_id,
                        seq,
                        &call_id,
                        Some(serde_json::json!({ "version": version })),
                    ).encode()?.into()
                )).await?;
            }

            // state.set_in — atomically set a nested field by JSON-pointer path
            // args: { state: "<key>", path: "/field/nested", value: <any> }
            "state.set_in" => {
                let state_key = match args.get("state").and_then(|v| v.as_str()) {
                    Some(k) => k.to_string(),
                    None => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, "state.set_in: missing 'state' key").encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };

                let path = match args.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, "state.set_in: missing 'path' key").encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };


                let new_val = args.get("value").cloned().unwrap_or(serde_json::Value::Null);
                let current = self.state.get(&state_key).map(|e| e.value).unwrap_or(serde_json::Value::Null);

                let updated = match json_set_in(current, &path, new_val) {
                    Ok(v) => v,
                    Err(msg) => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, &msg).encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };

                if let Some(schema) = &self.schema {
                    if let Err(msg) = schema.validate(&state_key, &updated) {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 422, &msg).encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                }

                let (version, ops) = self.state.apply(&state_key, updated.clone());
                metrics::gauge!("sodp_state_keys").set(self.state.key_count() as f64);
                if !ops.is_empty() {
                    let body_mp = encode_delta_body(version, &ops);
                    broadcast_timed(&self.fanout, &state_key, &body_mp, Some(&session.id));
                    if let Some(stream_id) = session.watch_stream_id(&state_key) {
                        let wire = frame::delta_bytes(stream_id, 0, &body_mp);
                        ws_tx.send(Message::Binary(wire.into())).await?;
                    }

                    if let Some(cluster) = &self.cluster {
                        cluster.sync(state_key.clone(), version, updated, body_mp);
                    }
                }

                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::result_ok(
                        frame.stream_id, seq, &call_id,
                        Some(serde_json::json!({ "version": version })),
                    ).encode()?.into()
                )).await?;
            }

            // state.delete — remove a key from the store entirely
            // args: { state: "<key>" }
            "state.delete" => {
                let state_key = match args.get("state").and_then(|v| v.as_str()) {
                    Some(k) => k.to_string(),
                    None => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, "state.delete: missing 'state' key").encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };


                let version = match self.state.delete(&state_key) {
                    Some((version, ops)) => {
                        metrics::gauge!("sodp_state_keys").set(self.state.key_count() as f64);
                        let body_mp = encode_delta_body(version, &ops);
                        broadcast_timed(&self.fanout, &state_key, &body_mp, Some(&session.id));
                        if let Some(stream_id) = session.watch_stream_id(&state_key) {
                            let wire = frame::delta_bytes(stream_id, 0, &body_mp);
                            ws_tx.send(Message::Binary(wire.into())).await?;
                        }
                        if let Some(cluster) = &self.cluster {
                            cluster.sync_delete(state_key.clone(), body_mp);
                        }
                        version
                    }
                    None => 0,
                };

                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::result_ok(
                        frame.stream_id, seq, &call_id,
                        Some(serde_json::json!({ "version": version })),
                    ).encode()?.into()
                )).await?;
            }

            // state.presence — set a nested path AND bind it to the session lifetime.
            // When the session closes for any reason the path is auto-removed and
            // a DELTA is broadcast to all watchers.
            // args: { state: "<key>", path: "/field", value: <any> }
            "state.presence" => {
                let state_key = match args.get("state").and_then(|v| v.as_str()) {
                    Some(k) => k.to_string(),
                    None => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, "state.presence: missing 'state' key").encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };

                let path = match args.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, "state.presence: missing 'path' key").encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };

                let new_val = args.get("value").cloned().unwrap_or(serde_json::Value::Null);
                let current = self.state.get(&state_key).map(|e| e.value).unwrap_or(serde_json::Value::Null);

                let updated = match json_set_in(current, &path, new_val) {
                    Ok(v)    => v,
                    Err(msg) => {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 400, &msg).encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                };

                if let Some(schema) = &self.schema {
                    if let Err(msg) = schema.validate(&state_key, &updated) {
                        let seq = session.next_seq();
                        ws_tx.send(Message::Binary(
                            frame::error(frame.stream_id, seq, 422, &msg).encode()?.into()
                        )).await?;
                        return Ok(());
                    }
                }

                let (version, ops) = self.state.apply(&state_key, updated.clone());
                metrics::gauge!("sodp_state_keys").set(self.state.key_count() as f64);
                if !ops.is_empty() {
                    let body_mp = encode_delta_body(version, &ops);
                    broadcast_timed(&self.fanout, &state_key, &body_mp, Some(&session.id));
                    if let Some(stream_id) = session.watch_stream_id(&state_key) {
                        let wire = frame::delta_bytes(stream_id, 0, &body_mp);
                        ws_tx.send(Message::Binary(wire.into())).await?;
                    }

                    if let Some(cluster) = &self.cluster {
                        cluster.sync(state_key.clone(), version, updated, body_mp);
                    }
                }

                // Register this (state_key, path) as session-owned.
                session.add_presence(state_key.clone(), path);

                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::result_ok(
                        frame.stream_id, seq, &call_id,
                        Some(serde_json::json!({ "version": version })),
                    ).encode()?.into()
                )).await?;
            }

            unknown => {
                warn!("Unknown method: {unknown}");
                let seq = session.next_seq();
                ws_tx.send(Message::Binary(
                    frame::error(
                        frame.stream_id,
                        seq,
                        404,
                        &format!("unknown method: {unknown}"),
                    ).encode()?.into()
                )).await?;
            }
        }

        Ok(())
    }
}

// --- helpers ---

/// Broadcast a pre-encoded delta body and record the fanout latency.
fn broadcast_timed(fanout: &FanoutBus, key: &str, body_mp: &[u8], exclude: Option<&str>) {
    let t0 = std::time::Instant::now();
    fanout.broadcast_encoded(key, body_mp, exclude);
    metrics::histogram!("sodp_delta_fanout_duration_ms")
        .record(t0.elapsed().as_secs_f64() * 1000.0);
}

fn extract_state_and_value(
    args: &serde_json::Value,
    ctx: &str,
) -> Result<(String, serde_json::Value), String> {
    let state_key = args
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("{ctx}: missing 'state' key"))?
        .to_string();
    let value = args.get("value").cloned().unwrap_or(serde_json::Value::Null);
    Ok((state_key, value))
}

/// Set a value at a JSON-pointer-style path, creating intermediate objects as needed.
/// `"/"` replaces the root value entirely.
fn json_set_in(root: serde_json::Value, path: &str, new_val: serde_json::Value) -> Result<serde_json::Value, String> {
    if path == "/" {
        return Ok(new_val);
    }
    if !path.starts_with('/') {
        return Err(format!("state.set_in: path must start with '/', got '{path}'"));
    }
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    Ok(set_in_recursive(root, &parts, new_val))
}

fn set_in_recursive(node: serde_json::Value, parts: &[&str], val: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    if parts.is_empty() {
        return val;
    }
    let key = parts[0].to_string();
    let mut map = match node {
        Value::Object(m) => m,
        _ => serde_json::Map::new(), // create intermediate object if non-object encountered
    };
    if parts.len() == 1 {
        map.insert(key, val);
    } else {
        let child = map.remove(&key).unwrap_or(Value::Null);
        let updated = set_in_recursive(child, &parts[1..], val);
        map.insert(key, updated);
    }
    Value::Object(map)
}

/// Remove the value at a JSON-pointer-style path, returning the modified root.
/// `"/"` or `""` nullifies the root.  Invalid or non-existent paths leave the
/// root unchanged.
fn json_remove_in(root: serde_json::Value, path: &str) -> serde_json::Value {
    if path.is_empty() || path == "/" {
        return serde_json::Value::Null;
    }
    if !path.starts_with('/') {
        return root;
    }
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    remove_in_recursive(root, &parts)
}

fn remove_in_recursive(node: serde_json::Value, parts: &[&str]) -> serde_json::Value {
    use serde_json::Value;
    let mut map = match node {
        Value::Object(m) => m,
        other            => return other, // non-object — can't traverse, leave unchanged
    };
    if parts.len() == 1 {
        map.remove(parts[0]);
    } else if let Some(child) = map.remove(parts[0]) {
        let updated = remove_in_recursive(child, &parts[1..]);
        map.insert(parts[0].to_string(), updated);
    }
    Value::Object(map)
}

/// Recursively merge `patch` into `base`.
/// Scalar/array values in `patch` overwrite `base`; objects are merged recursively.
fn json_merge(base: serde_json::Value, patch: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match (base, patch) {
        (Value::Object(mut base_map), Value::Object(patch_map)) => {
            for (k, v) in patch_map {
                let existing = base_map.remove(&k).unwrap_or(Value::Null);
                base_map.insert(k, json_merge(existing, v));
            }
            Value::Object(base_map)
        }
        (_, patch) => patch,
    }
}
