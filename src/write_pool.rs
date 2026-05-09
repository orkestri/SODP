use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures_util::SinkExt;
use futures_util::stream::SplitSink;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::frame::{self, OutboundMsg};

// ── WriteHandle ───────────────────────────────────────────────────────────────

/// Per-session write endpoint. Works in both task mode and pool mode.
pub struct WriteHandle {
    pub cancel: CancellationToken,
    inner: Inner,
}

enum Inner {
    /// Per-session write task owns the sink; no locking, maximum parallelism.
    Task { outbound_tx: mpsc::Sender<OutboundMsg> },
    /// Shared pool of workers drain per-session queues.
    Pool(Box<PoolFields>),
}

struct PoolFields {
    outbound_tx:    mpsc::Sender<OutboundMsg>,
    outbound_rx:    Mutex<mpsc::Receiver<OutboundMsg>>,
    ws_tx:          Mutex<SplitSink<WebSocketStream<TcpStream>, Message>>,
    pool_queue_tx:  mpsc::UnboundedSender<Arc<WriteHandle>>,
    in_pool:        AtomicBool,
    pending:        AtomicUsize,
}

impl WriteHandle {
    fn new_task(
        ws_tx: SplitSink<WebSocketStream<TcpStream>, Message>,
        backpressure: usize,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        let (outbound_tx, outbound_rx) = mpsc::channel(backpressure);
        let handle = Arc::new(WriteHandle {
            cancel: cancel.clone(),
            inner: Inner::Task { outbound_tx },
        });
        tokio::spawn(write_task(outbound_rx, ws_tx, cancel));
        handle
    }

    /// Enqueue a message for delivery. Returns `false` (and cancels the session) on overflow.
    pub fn send(self: &Arc<Self>, msg: OutboundMsg) -> bool {
        match &self.inner {
            Inner::Task { outbound_tx } => {
                match outbound_tx.try_send(msg) {
                    Ok(()) => true,
                    Err(_) => { self.cancel.cancel(); false }
                }
            }
            Inner::Pool(p) => {
                p.pending.fetch_add(1, Ordering::AcqRel);
                if p.outbound_tx.try_send(msg).is_err() {
                    p.pending.fetch_sub(1, Ordering::AcqRel);
                    self.cancel.cancel();
                    return false;
                }
                self.pool_enqueue();
                true
            }
        }
    }

    fn pool_enqueue(self: &Arc<Self>) {
        if let Inner::Pool(p) = &self.inner {
            if p.in_pool
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                let _ = p.pool_queue_tx.send(Arc::clone(self));
            }
        }
    }
}

// ── Task-mode write loop ──────────────────────────────────────────────────────

async fn write_task(
    mut rx: mpsc::Receiver<OutboundMsg>,
    mut ws: SplitSink<WebSocketStream<TcpStream>, Message>,
    cancel: CancellationToken,
) {
    loop {
        let msg = tokio::select! {
            msg = rx.recv() => match msg { Some(m) => m, None => break },
            _ = cancel.cancelled() => break,
        };
        if let Some(wm) = to_ws_msg(msg) {
            if ws.feed(wm).await.is_err() { break; }
        }
        // Coalesce: drain any queued messages before flushing.
        while let Ok(m) = rx.try_recv() {
            if let Some(wm) = to_ws_msg(m) {
                if ws.feed(wm).await.is_err() { break; }
            }
        }
        if ws.flush().await.is_err() { break; }
    }
    let _ = ws.close().await;
}

// ── WritePool (pool mode) ─────────────────────────────────────────────────────

struct WritePool {
    queue_tx: mpsc::UnboundedSender<Arc<WriteHandle>>,
}

impl WritePool {
    fn new(pool_size: usize) -> Arc<Self> {
        let (queue_tx, queue_rx) = mpsc::unbounded_channel::<Arc<WriteHandle>>();
        let queue_rx = Arc::new(Mutex::new(queue_rx));

        for _ in 0..pool_size {
            let rx = Arc::clone(&queue_rx);
            tokio::spawn(async move {
                loop {
                    let handle = {
                        let mut locked = rx.lock().await;
                        match locked.recv().await {
                            Some(h) => h,
                            None    => break,
                        }
                    };
                    pool_drain(&handle).await;
                }
            });
        }

        Arc::new(WritePool { queue_tx })
    }

    fn new_handle(
        &self,
        ws_tx: SplitSink<WebSocketStream<TcpStream>, Message>,
        backpressure: usize,
        cancel: CancellationToken,
    ) -> Arc<WriteHandle> {
        let (outbound_tx, outbound_rx) = mpsc::channel(backpressure);
        Arc::new(WriteHandle {
            cancel,
            inner: Inner::Pool(Box::new(PoolFields {
                outbound_tx,
                outbound_rx:   Mutex::new(outbound_rx),
                ws_tx:         Mutex::new(ws_tx),
                pool_queue_tx: self.queue_tx.clone(),
                in_pool:       AtomicBool::new(false),
                pending:       AtomicUsize::new(0),
            })),
        })
    }
}

async fn pool_drain(handle: &Arc<WriteHandle>) {
    if let Inner::Pool(p) = &handle.inner {
        let mut rx = p.outbound_rx.lock().await;
        let mut ws = p.ws_tx.lock().await;

        while let Ok(msg) = rx.try_recv() {
            p.pending.fetch_sub(1, Ordering::AcqRel);
            if let Some(m) = to_ws_msg(msg) {
                if ws.feed(m).await.is_err() { break; }
            }
        }
        let _ = ws.flush().await;

        drop(rx);
        drop(ws);

        p.in_pool.store(false, Ordering::Release);
        if p.pending.load(Ordering::Acquire) > 0 {
            handle.pool_enqueue();
        }
    }
}

// ── WriteMode ─────────────────────────────────────────────────────────────────

/// Write strategy, selected at server startup via `SODP_WRITE_MODE`.
///
/// | Mode   | When to prefer |
/// |--------|----------------|
/// | `task` | Default. Best at ≥10 subscribers; one tokio task per session owns the sink. |
/// | `pool` | Consider for zero/low fanout workloads; shared CORES×2 (or `SODP_WRITE_POOL_SIZE`) workers. |
pub struct WriteMode {
    inner: WriteModeInner,
}

enum WriteModeInner {
    Task,
    Pool(Arc<WritePool>),
}

impl WriteMode {
    /// Read `SODP_WRITE_MODE` and `SODP_WRITE_POOL_SIZE` from the environment.
    pub fn from_env() -> Self {
        let mode = std::env::var("SODP_WRITE_MODE").unwrap_or_default();
        if mode.eq_ignore_ascii_case("pool") {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4);
            let pool_size = std::env::var("SODP_WRITE_POOL_SIZE")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(cores * 2)
                .max(2);
            tracing::info!("Write mode: pool ({pool_size} workers)");
            WriteMode { inner: WriteModeInner::Pool(WritePool::new(pool_size)) }
        } else {
            tracing::info!("Write mode: task (per-session)");
            WriteMode { inner: WriteModeInner::Task }
        }
    }

    pub fn make_handle(
        &self,
        ws_tx: SplitSink<WebSocketStream<TcpStream>, Message>,
        backpressure: usize,
        cancel: CancellationToken,
    ) -> Arc<WriteHandle> {
        match &self.inner {
            WriteModeInner::Task       => WriteHandle::new_task(ws_tx, backpressure, cancel),
            WriteModeInner::Pool(pool) => pool.new_handle(ws_tx, backpressure, cancel),
        }
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

fn to_ws_msg(outbound: OutboundMsg) -> Option<Message> {
    match outbound {
        OutboundMsg::Frame(f) => match f.encode() {
            Ok(bytes) => Some(Message::Binary(bytes)),
            Err(e)    => { warn!("Frame encode error: {e}"); None }
        }
        OutboundMsg::Bytes(b) => Some(Message::Binary(b)),
        OutboundMsg::ArcDelta { stream_id, body_mp } =>
            Some(Message::Binary(frame::delta_bytes(stream_id, 0, &body_mp))),
        OutboundMsg::Ping       => Some(Message::Ping(vec![])),
        OutboundMsg::Pong(data) => Some(Message::Pong(data)),
    }
}
