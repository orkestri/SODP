//! SODP vs REST (SSE) vs gRPC — benchmark
//!
//! Scenario: 11-field object, only the `counter` field changes each update.
//!
//! Measured per technology:
//!   • P50 / P99 latency (µs) — from "mutation sent" to "client receives update"
//!   • Bytes per update       — raw bytes the client receives to learn of a change
//!   • Throughput             — effective mutations/sec (sequential, single client)
//!
//! REST uses SSE (Server-Sent Events) for push — the closest REST analog to
//! SODP's WATCH stream.  gRPC uses server-side streaming.  Both send the FULL
//! state on every mutation; SODP sends only the changed fields (delta).

#![allow(clippy::type_complexity)]

#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use dashmap::DashMap;
use futures_util::{SinkExt, Stream, StreamExt};
use prost::Message as _; // bring encode_to_vec into scope
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::{Duration, sleep};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMsg;

use sodp::frame::types;
use sodp::server::SodpServer;

// ─── proto-generated types ────────────────────────────────────────────────────

pub mod pb {
    tonic::include_proto!("bench");
}

use pb::state_service_client::StateServiceClient;
use pb::state_service_server::{StateService, StateServiceServer};
use pb::{SetStateRequest, SetStateResponse, StateEvent, WatchRequest};

// ─── benchmark workload ───────────────────────────────────────────────────────

/// 11-field object. Only `counter` changes between iterations.
/// All other fields are fixed 20-char strings → realistic real-world payload.
fn test_object(n: u64) -> Value {
    serde_json::json!({
        "counter": n,
        "field_a": "aaaaaaaaaaaaaaaaaaaa",
        "field_b": "bbbbbbbbbbbbbbbbbbbb",
        "field_c": "cccccccccccccccccccc",
        "field_d": "dddddddddddddddddddd",
        "field_e": "eeeeeeeeeeeeeeeeeeee",
        "field_f": "ffffffffffffffffffff",
        "field_g": "gggggggggggggggggggg",
        "field_h": "hhhhhhhhhhhhhhhhhhhh",
        "field_i": "iiiiiiiiiiiiiiiiiiii",
        "field_j": "jjjjjjjjjjjjjjjjjjjj"
    })
}

// ─── REST server ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct RestState {
    store: Arc<DashMap<String, Vec<u8>>>,
    version: Arc<AtomicU64>,
    /// Broadcast: (key, version, full_json_bytes)
    tx: broadcast::Sender<(String, u64, Vec<u8>)>,
}

async fn rest_set_state(
    Path(key): Path<String>,
    State(state): State<RestState>,
    body: Bytes,
) -> Json<Value> {
    let v = state.version.fetch_add(1, Ordering::SeqCst) + 1;
    state.store.insert(key.clone(), body.to_vec());
    let _ = state.tx.send((key, v, body.to_vec()));
    Json(serde_json::json!({ "version": v }))
}

async fn rest_sse_state(
    Path(key): Path<String>,
    State(state): State<RestState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.tx.subscribe();
    let stream = futures_util::stream::unfold((rx, key), |(mut rx, key)| async move {
        loop {
            match rx.recv().await {
                Ok((k, _, val)) if k == key => {
                    let data = String::from_utf8(val).unwrap_or_default();
                    return Some((Ok(Event::default().data(data)), (rx, key)));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });
    Sse::new(stream)
}

async fn start_rest_server(addr: &'static str) {
    let (tx, _) = broadcast::channel(1024);
    let state = RestState {
        store: Arc::new(DashMap::new()),
        version: Arc::new(AtomicU64::new(0)),
        tx,
    };
    let app = Router::new()
        .route("/state/:key", post(rest_set_state))
        .route("/state/:key/events", get(rest_sse_state))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ─── gRPC server ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct GrpcService {
    store: Arc<DashMap<String, Vec<u8>>>,
    version: Arc<AtomicU64>,
    tx: broadcast::Sender<(String, u64, Vec<u8>)>,
}

#[tonic::async_trait]
impl StateService for GrpcService {
    async fn set_state(
        &self,
        req: tonic::Request<SetStateRequest>,
    ) -> Result<tonic::Response<SetStateResponse>, tonic::Status> {
        let r = req.into_inner();
        let v = self.version.fetch_add(1, Ordering::SeqCst) + 1;
        self.store.insert(r.key.clone(), r.value.clone());
        let _ = self.tx.send((r.key, v, r.value));
        Ok(tonic::Response::new(SetStateResponse { version: v }))
    }

    type WatchStateStream = std::pin::Pin<
        Box<dyn futures_util::Stream<Item = Result<StateEvent, tonic::Status>> + Send + 'static>,
    >;

    async fn watch_state(
        &self,
        req: tonic::Request<WatchRequest>,
    ) -> Result<tonic::Response<Self::WatchStateStream>, tonic::Status> {
        let key = req.into_inner().key;
        let mut rx = self.tx.subscribe();
        let (inner_tx, inner_rx) =
            tokio::sync::mpsc::channel::<Result<StateEvent, tonic::Status>>(256);

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok((k, v, val)) if k == key => {
                        let ev = StateEvent {
                            key: k,
                            version: v,
                            value: val,
                        };
                        if inner_tx.send(Ok(ev)).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(inner_rx);
        Ok(tonic::Response::new(Box::pin(stream)))
    }
}

async fn start_grpc_server(addr: &'static str) {
    let (tx, _) = broadcast::channel(1024);
    let svc = GrpcService {
        store: Arc::new(DashMap::new()),
        version: Arc::new(AtomicU64::new(0)),
        tx,
    };
    tonic::transport::Server::builder()
        .add_service(StateServiceServer::new(svc))
        .serve(SocketAddr::from_str(addr).unwrap())
        .await
        .unwrap();
}

// ─── results & statistics ─────────────────────────────────────────────────────

struct BenchResult {
    name: String,
    latencies_us: Vec<u128>,
    bytes_per_update: Vec<usize>,
}

impl BenchResult {
    fn p50(&self) -> u128 {
        percentile(&self.latencies_us, 50)
    }
    fn p99(&self) -> u128 {
        percentile(&self.latencies_us, 99)
    }

    fn avg_bytes(&self) -> usize {
        if self.bytes_per_update.is_empty() {
            return 0;
        }
        self.bytes_per_update.iter().sum::<usize>() / self.bytes_per_update.len()
    }

    /// Sequential throughput: total mutations / total wall time
    fn kops(&self) -> f64 {
        let total_us: u128 = self.latencies_us.iter().sum();
        if total_us == 0 {
            return 0.0;
        }
        (self.latencies_us.len() as f64) / (total_us as f64 / 1_000_000.0) / 1_000.0
    }
}

fn percentile(data: &[u128], p: usize) -> u128 {
    if data.is_empty() {
        return 0;
    }
    let mut sorted = data.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() * p / 100).min(sorted.len() - 1)]
}

// ─── SODP benchmark client ────────────────────────────────────────────────────
//
// Architecture mirrors REST SSE:
//   • watcher  connection — subscribes to the key, receives DELTA push notifications
//   • writer   connection — sends CALL mutations, receives RESULT confirmations
//
// Latency = time from CALL-sent (writer) until DELTA-received (watcher).
// This is the same measurement REST makes: POST-sent → SSE-event-received.
// Bytes/update = DELTA frame bytes (only the changed fields).

async fn bench_sodp(iters: usize) -> BenchResult {
    // ── watcher connection ────────────────────────────────────────────────────
    let (mut watcher_ws, _) = connect_async("ws://127.0.0.1:7777")
        .await
        .expect("sodp watcher connect");
    watcher_ws.next().await; // HELLO

    let watch = sodp::frame::Frame {
        frame_type: types::WATCH,
        stream_id: 0,
        seq: 0,
        body: serde_json::json!({ "state": "bench.sodp" }),
    };
    watcher_ws
        .send(WsMsg::Binary(watch.encode().unwrap()))
        .await
        .unwrap();
    watcher_ws.next().await; // STATE_INIT

    // Watcher task: receives DELTAs and reports (recv_time, bytes) to the main task.
    let (report_tx, mut report_rx) = tokio::sync::mpsc::unbounded_channel::<(Instant, usize)>();
    {
        let tx = report_tx.clone();
        tokio::spawn(async move {
            while let Some(Ok(WsMsg::Binary(bytes))) = watcher_ws.next().await {
                let n = bytes.len();
                let _ = tx.send((Instant::now(), n));
            }
        });
    }

    // ── writer connection ─────────────────────────────────────────────────────
    // Writer does NOT watch, so each CALL produces exactly one RESULT (no DELTA).
    let (mut writer_ws, _) = connect_async("ws://127.0.0.1:7777")
        .await
        .expect("sodp writer connect");
    writer_ws.next().await; // HELLO

    let mut latencies_us = Vec::with_capacity(iters);
    let mut bytes_per_update = Vec::with_capacity(iters);

    for i in 0..iters as u64 {
        let call = sodp::frame::Frame {
            frame_type: types::CALL,
            stream_id: 0,
            seq: i,
            body: serde_json::json!({
                "call_id": i,
                "method":  "state.set",
                "args":    { "state": "bench.sodp", "value": test_object(i) }
            }),
        };

        let send_time = Instant::now();
        writer_ws
            .send(WsMsg::Binary(call.encode().unwrap()))
            .await
            .unwrap();

        // Wait for the watcher to receive the DELTA — this is the push-notification
        // latency, directly comparable to REST's POST → SSE-event measurement.
        if let Some((recv_time, n)) = report_rx.recv().await {
            latencies_us.push(recv_time.duration_since(send_time).as_micros());
            bytes_per_update.push(n);
        }

        // Consume RESULT on the writer (outside the timing path).
        writer_ws.next().await;
    }

    BenchResult {
        name: "SODP (delta)".into(),
        latencies_us,
        bytes_per_update,
    }
}

// ─── REST SSE benchmark client ────────────────────────────────────────────────

async fn bench_rest(iters: usize) -> BenchResult {
    let client = reqwest::Client::new();
    let set_url = "http://127.0.0.1:7778/state/bench.rest";
    let sse_url = "http://127.0.0.1:7778/state/bench.rest/events";

    // Establish SSE subscription before the loop
    let sse_resp = client.get(sse_url).send().await.expect("sse connect");
    let mut sse = sse_resp.bytes_stream();

    let mut latencies_us = Vec::with_capacity(iters);
    let mut bytes_per_update = Vec::with_capacity(iters);

    for i in 0..iters as u64 {
        let body = serde_json::to_vec(&test_object(i)).unwrap();

        let t = Instant::now();
        client
            .post(set_url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();

        // Collect bytes until we see the blank-line SSE event terminator "\n\n"
        let mut buf = Vec::new();
        while let Some(Ok(chunk)) = sse.next().await {
            buf.extend_from_slice(&chunk);
            if buf.windows(2).any(|w| w[0] == b'\n' && w[1] == b'\n') {
                latencies_us.push(t.elapsed().as_micros());
                bytes_per_update.push(buf.len());
                buf.clear();
                break;
            }
        }
    }

    BenchResult {
        name: "REST (SSE)".into(),
        latencies_us,
        bytes_per_update,
    }
}

// ─── gRPC streaming benchmark client ─────────────────────────────────────────

async fn bench_grpc(iters: usize) -> BenchResult {
    let mut client = StateServiceClient::connect("http://127.0.0.1:7779")
        .await
        .expect("grpc connect");

    let mut stream = client
        .watch_state(WatchRequest {
            key: "bench.grpc".into(),
        })
        .await
        .unwrap()
        .into_inner();

    let mut latencies_us = Vec::with_capacity(iters);
    let mut bytes_per_update = Vec::with_capacity(iters);

    for i in 0..iters as u64 {
        let req = SetStateRequest {
            key: "bench.grpc".into(),
            value: serde_json::to_vec(&test_object(i)).unwrap(),
        };

        let t = Instant::now();
        client.set_state(req).await.unwrap();

        if let Some(Ok(event)) = stream.next().await {
            latencies_us.push(t.elapsed().as_micros());
            // Count bytes as if transmitted over the wire (protobuf encoding)
            bytes_per_update.push(event.encode_to_vec().len());
        }
    }

    BenchResult {
        name: "gRPC (stream)".into(),
        latencies_us,
        bytes_per_update,
    }
}

// ─── scalability bench helpers ───────────────────────────────────────────────

/// Adaptive round count: fewer rounds at higher watcher counts to keep total
/// runtime reasonable while still producing stable numbers.
fn scale_rounds(watchers: usize, quick: bool) -> usize {
    if quick {
        match watchers {
            0 => 200,
            1..=100 => 30,
            101..=500 => 10,
            _ => 5,
        }
    } else {
        match watchers {
            0 => 500,
            1..=10 => 200,
            11..=100 => 100,
            101..=500 => 30,
            _ => 15,
        }
    }
}

/// Baseline: writer only, no fanout subscribers.
/// Measures CALL → RESULT round-trip latency and derives ops/sec.
async fn bench_baseline_sodp(url: &str, rounds: usize) -> ScaleRow {
    let (mut ws, _) = connect_async(url).await.unwrap();
    ws.next().await; // HELLO

    let mut latencies: Vec<u128> = Vec::with_capacity(rounds);
    for i in 0..rounds as u64 {
        let call = sodp::frame::Frame {
            frame_type: types::CALL,
            stream_id: 0,
            seq: i,
            body: serde_json::json!({
                "call_id": i, "method": "state.set",
                "args": { "state": "bench.scale.base", "value": test_object(i) },
            }),
        };
        let t = Instant::now();
        ws.send(WsMsg::Binary(call.encode().unwrap()))
            .await
            .unwrap();
        ws.next().await; // RESULT
        latencies.push(t.elapsed().as_micros());
    }

    let total_us: u128 = latencies.iter().sum();
    let ops_sec = if total_us > 0 {
        1_000_000.0 * rounds as f64 / total_us as f64
    } else {
        0.0
    };
    ScaleRow {
        watchers: 0,
        conns: 0,
        ops_sec,
        p50_us: percentile(&latencies, 50),
        p99_us: percentile(&latencies, 99),
    }
}

/// One row of the scalability table for `watchers > 0`.
async fn bench_scale_row(url: &str, watchers: usize, mux: usize, rounds: usize) -> ScaleRow {
    let conns = watchers.div_ceil(mux.max(1));
    let r = bench_fanout_sodp_at(url, watchers, mux, rounds).await;
    let total_us: u128 = r.max_latencies_us.iter().sum();
    let ops_sec = if total_us > 0 {
        1_000_000.0 * rounds as f64 / total_us as f64
    } else {
        0.0
    };
    ScaleRow {
        watchers,
        conns,
        ops_sec,
        p50_us: r.p50(),
        p99_us: r.p99(),
    }
}

/// Run the full scalability sweep for the given watcher counts and mux factor.
async fn bench_scale_sweep(
    url: &str,
    watcher_counts: &[usize],
    mux: usize,
    quick: bool,
) -> Vec<ScaleRow> {
    let mut rows = Vec::new();
    for &w in watcher_counts {
        let rounds = scale_rounds(w, quick);
        // Brief warmup before each measurement.
        if w == 0 {
            bench_baseline_sodp(url, 20).await;
        } else {
            bench_fanout_sodp_at(url, w, mux, 5).await;
        }
        let row = if w == 0 {
            bench_baseline_sodp(url, rounds).await
        } else {
            bench_scale_row(url, w, mux, rounds).await
        };
        rows.push(row);
    }
    rows
}

// ─── output helpers ───────────────────────────────────────────────────────────

/// Index of the minimum value (used to find the winner for "less is better" metrics).
fn winner_min(vals: &[u128]) -> usize {
    vals.iter()
        .enumerate()
        .min_by_key(|(_, v)| *v)
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Index of the minimum value for usize slices.
fn winner_min_usize(vals: &[usize]) -> usize {
    vals.iter()
        .enumerate()
        .min_by_key(|(_, v)| *v)
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Index of the maximum value (used for "more is better" metrics like throughput).
fn winner_max_f64(vals: &[f64]) -> usize {
    vals.iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Format a value; append " ★" when it is the winner.
fn cell(val: impl std::fmt::Display, is_winner: bool) -> String {
    if is_winner {
        format!("{val} ★")
    } else {
        val.to_string()
    }
}

/// Format a µs latency value as "NNNµs", "N.Nms", or "N.Ns".
fn fmt_lat(us: u128) -> String {
    if us < 1_000 {
        format!("{}µs", us)
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    }
}

/// Format ops/sec with comma thousands separator ("52,559", "1,850", "380").
fn fmt_ops(ops: f64) -> String {
    let s = (ops.round() as u64).to_string();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn print_scale_table(title: &str, rows: &[ScaleRow]) {
    println!("\n  {title}");
    println!(
        "  {:>8}  {:>6}  {:>12}  {:>10}  {:>10}",
        "watchers", "conns", "ops/sec", "P50", "P99"
    );
    println!(
        "  {:─<8}  {:─<6}  {:─<12}  {:─<10}  {:─<10}",
        "", "", "", "", ""
    );
    for r in rows {
        println!(
            "  {:>8}  {:>6}  {:>12}  {:>10}  {:>10}",
            r.watchers,
            r.conns,
            fmt_ops(r.ops_sec),
            fmt_lat(r.p50_us),
            fmt_lat(r.p99_us),
        );
    }
}

fn print_scale_comparison(title: &str, rust: &[ScaleRow], go: &[ScaleRow]) {
    println!("\n  {title}");
    println!(
        "  {:>8}  {:>6} │ {:>10} {:>8} {:>8} │ {:>10} {:>8} {:>8}",
        "watchers", "conns", "Rust op/s", "P50", "P99", "Go op/s", "P50", "P99",
    );
    println!(
        "  {:─<8}  {:─<6}─┼─{:─<10}─{:─<8}─{:─<8}─┼─{:─<10}─{:─<8}─{:─<8}",
        "", "", "", "", "", "", "", "",
    );
    let len = rust.len().max(go.len());
    for i in 0..len {
        let (rw, rc) = rust.get(i).map(|r| (r.watchers, r.conns)).unwrap_or((0, 0));
        let (ro, rp50, rp99) = rust
            .get(i)
            .map(|r| (fmt_ops(r.ops_sec), fmt_lat(r.p50_us), fmt_lat(r.p99_us)))
            .unwrap_or_else(|| ("—".into(), "—".into(), "—".into()));
        let (go_o, go_p50, go_p99) = go
            .get(i)
            .map(|r| (fmt_ops(r.ops_sec), fmt_lat(r.p50_us), fmt_lat(r.p99_us)))
            .unwrap_or_else(|| ("—".into(), "—".into(), "—".into()));
        println!(
            "  {:>8}  {:>6} │ {:>10} {:>8} {:>8} │ {:>10} {:>8} {:>8}",
            rw, rc, ro, rp50, rp99, go_o, go_p50, go_p99,
        );
    }
}

// ─── output ───────────────────────────────────────────────────────────────────

fn print_results(results: &[BenchResult]) {
    let p50s: Vec<u128> = results.iter().map(|r| r.p50()).collect();
    let p99s: Vec<u128> = results.iter().map(|r| r.p99()).collect();
    let bytes: Vec<usize> = results.iter().map(|r| r.avg_bytes()).collect();
    let kops: Vec<f64> = results.iter().map(|r| r.kops()).collect();

    let wi_p50 = winner_min(&p50s);
    let wi_p99 = winner_min(&p99s);
    let wi_bytes = winner_min_usize(&bytes);
    let wi_kops = winner_max_f64(&kops);

    println!(
        "\n┌{0}┬{1}┬{1}┬{2}┬{3}┐",
        "─".repeat(18),
        "─".repeat(14),
        "─".repeat(15),
        "─".repeat(14),
    );
    println!(
        "│ {:<16} │ {:>12} │ {:>12} │ {:>13} │ {:>12} │",
        "Technology", "P50 µs (↓)", "P99 µs (↓)", "Bytes/upd (↓)", "Kops/s (↑)"
    );
    println!(
        "├{0}┼{1}┼{1}┼{2}┼{3}┤",
        "─".repeat(18),
        "─".repeat(14),
        "─".repeat(15),
        "─".repeat(14),
    );
    for (i, r) in results.iter().enumerate() {
        println!(
            "│ {:<16} │ {:>12} │ {:>12} │ {:>13} │ {:>12} │",
            r.name,
            cell(r.p50(), i == wi_p50),
            cell(r.p99(), i == wi_p99),
            cell(r.avg_bytes(), i == wi_bytes),
            cell(format!("{:.1}", r.kops()), i == wi_kops),
        );
    }
    println!(
        "└{0}┴{1}┴{1}┴{2}┴{3}┘",
        "─".repeat(18),
        "─".repeat(14),
        "─".repeat(15),
        "─".repeat(14),
    );

    println!("\n  ↓ less is better   ↑ more is better   ★ = winner\n");
    println!(
        "  P50 latency   (↓ less is better)  →  {} {} µs",
        results[wi_p50].name, p50s[wi_p50]
    );
    println!(
        "  P99 latency   (↓ less is better)  →  {} {} µs",
        results[wi_p99].name, p99s[wi_p99]
    );
    println!(
        "  Bandwidth     (↓ less is better)  →  {} {} B  ({:.1}× less than runner-up)",
        results[wi_bytes].name,
        bytes[wi_bytes],
        bytes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != wi_bytes)
            .map(|(_, &b)| b as f64 / bytes[wi_bytes] as f64)
            .fold(0f64, f64::max)
    );
    println!(
        "  Throughput    (↑ more is better)  →  {} {:.1} kops/s",
        results[wi_kops].name, kops[wi_kops]
    );
}

// ─── fanout benchmark ─────────────────────────────────────────────────────────
//
// N watcher clients all subscribe to the same state key.
// One writer sends a mutation.
// We measure the time until the LAST watcher receives the update,
// and the total bytes pushed to ALL watchers combined.

struct FanoutResult {
    name: String,
    /// Per round: microseconds from mutation-sent until all watchers received.
    max_latencies_us: Vec<u128>,
    /// Per round: sum of bytes received across ALL watchers.
    total_bytes_per_round: Vec<usize>,
}

// ─── scalability sweep ────────────────────────────────────────────────────────

struct ScaleRow {
    watchers: usize,
    conns: usize,
    ops_sec: f64,
    p50_us: u128,
    p99_us: u128,
}

impl FanoutResult {
    fn p50(&self) -> u128 {
        percentile(&self.max_latencies_us, 50)
    }
    fn p99(&self) -> u128 {
        percentile(&self.max_latencies_us, 99)
    }

    fn avg_total_bytes(&self) -> usize {
        if self.total_bytes_per_round.is_empty() {
            return 0;
        }
        self.total_bytes_per_round.iter().sum::<usize>() / self.total_bytes_per_round.len()
    }
}

/// Position of the first `\n\n` in `buf`, or `None`.
fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w[0] == b'\n' && w[1] == b'\n')
}

/// Collect exactly `want` receipts from `rx`, recording each (recv_time, bytes).
/// Returns (max_latency_us, total_bytes). Times out per-receipt at 5 s.
async fn collect_receipts(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<(Instant, usize)>,
    want: usize,
    send_time: Instant,
) -> (u128, usize) {
    let mut max_us = 0u128;
    let mut total = 0usize;
    for _ in 0..want {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some((recv_time, bytes))) => {
                let lat = recv_time.duration_since(send_time).as_micros();
                if lat > max_us {
                    max_us = lat;
                }
                total += bytes;
            }
            _ => break,
        }
    }
    (max_us, total)
}

// ── SODP fanout ───────────────────────────────────────────────────────────────
//
// mux > 1 opens fewer physical WebSocket connections, each carrying multiple
// independent WATCH subscriptions (different stream_ids).  The server delivers
// one DELTA per subscription per mutation, so the total report count per round
// is unchanged (`watchers`).  Fewer connections means less OS/scheduler
// overhead, tighter TCP coalescing, and far fewer tokio tasks.

async fn bench_fanout_sodp_at(
    url: &str,
    watchers: usize,
    mux: usize,
    rounds: usize,
) -> FanoutResult {
    let mux = mux.max(1);
    let conns = watchers.div_ceil(mux); // physical WebSocket connections
    let (report_tx, mut report_rx) = tokio::sync::mpsc::unbounded_channel::<(Instant, usize)>();

    for c in 0..conns {
        let tx = report_tx.clone();
        let url = url.to_owned();
        // Last connection may carry fewer subscriptions if watchers % mux != 0.
        let subs = if (c + 1) * mux <= watchers {
            mux
        } else {
            watchers - c * mux
        };
        tokio::spawn(async move {
            let (mut ws, _) = connect_async(&url).await.unwrap();
            ws.next().await; // HELLO

            // Open `subs` independent WATCH subscriptions; each gets a unique
            // stream_id allocated by the server (we send stream_id=0).
            for _ in 0..subs {
                let watch = sodp::frame::Frame {
                    frame_type: types::WATCH,
                    stream_id: 0,
                    seq: 0,
                    body: serde_json::json!({ "state": "fanout.sodp" }),
                };
                ws.send(WsMsg::Binary(watch.encode().unwrap()))
                    .await
                    .unwrap();
                ws.next().await; // STATE_INIT for this subscription
            }

            // After setup every incoming binary frame is a DELTA (one per sub per mutation).
            while let Some(Ok(WsMsg::Binary(bytes))) = ws.next().await {
                let _ = tx.send((Instant::now(), bytes.len()));
            }
        });
    }
    drop(report_tx);
    // Scale sleep with connection count: each connection needs connect+HELLO+N×(WATCH+STATE_INIT).
    let setup_ms = (conns as u64 * 6).max(600);
    sleep(Duration::from_millis(setup_ms)).await;

    let (mut writer, _) = connect_async(url).await.unwrap();
    writer.next().await; // HELLO  (writer never WATCHes — only sends mutations)

    let mut max_latencies_us = Vec::with_capacity(rounds);
    let mut total_bytes_per_round = Vec::with_capacity(rounds);

    for r in 0..rounds as u64 {
        let call = sodp::frame::Frame {
            frame_type: types::CALL,
            stream_id: 0,
            seq: r,
            body: serde_json::json!({
                "call_id": r,
                "method":  "state.set",
                "args":    { "state": "fanout.sodp", "value": test_object(r) }
            }),
        };

        let send_time = Instant::now();
        writer
            .send(WsMsg::Binary(call.encode().unwrap()))
            .await
            .unwrap();
        writer.next().await; // consume RESULT

        // collect_receipts expects exactly `watchers` reports:
        // conns connections × subs subscriptions each = watchers total.
        let (max_us, total_bytes) = collect_receipts(&mut report_rx, watchers, send_time).await;
        max_latencies_us.push(max_us);
        total_bytes_per_round.push(total_bytes);
    }

    let label = if mux == 1 {
        "SODP (delta)".into()
    } else {
        format!("mux={mux} ({conns} conns)")
    };
    FanoutResult {
        name: label,
        max_latencies_us,
        total_bytes_per_round,
    }
}

async fn bench_fanout_sodp(watchers: usize, mux: usize, rounds: usize) -> FanoutResult {
    bench_fanout_sodp_at("ws://127.0.0.1:7777", watchers, mux, rounds).await
}

// ── SODP mux matrix ───────────────────────────────────────────────────────────

struct MuxResult {
    mux: usize,
    conns: usize,
    r: FanoutResult,
}

async fn bench_mux_matrix(watchers: usize, rounds: usize) -> Vec<MuxResult> {
    // Candidate mux values: 1, 5, 10, 50, 100 — filtered to ≤ watchers.
    let candidates: Vec<usize> = [1, 5, 10, 50, 100]
        .into_iter()
        .filter(|&m| m <= watchers)
        .collect();

    let mut out = Vec::new();
    for &mux in &candidates {
        let conns = watchers.div_ceil(mux);
        // Warmup
        bench_fanout_sodp(watchers, mux, 30).await;
        let r = bench_fanout_sodp(watchers, mux, rounds).await;
        out.push(MuxResult { mux, conns, r });
    }
    out
}

fn print_mux_matrix(results: &[MuxResult], watchers: usize, rounds: usize) {
    let p50s: Vec<u128> = results.iter().map(|r| r.r.p50()).collect();
    let p99s: Vec<u128> = results.iter().map(|r| r.r.p99()).collect();
    let wi_p50 = winner_min(&p50s);
    let wi_p99 = winner_min(&p99s);

    println!(
        "\n── SODP mux matrix: {watchers} logical watchers, 1 writer, {rounds} rounds ─────────────"
    );
    println!("   Each row is the same {watchers} subscriptions on fewer physical connections.\n");
    println!(
        "┌{0}┬{1}┬{2}┬{2}┐",
        "─".repeat(10),
        "─".repeat(12),
        "─".repeat(14),
    );
    println!(
        "│ {:<8} │ {:>10} │ {:>12} │ {:>12} │",
        "mux", "conns", "P50 µs (↓)", "P99 µs (↓)",
    );
    println!(
        "├{0}┼{1}┼{2}┼{2}┤",
        "─".repeat(10),
        "─".repeat(12),
        "─".repeat(14),
    );
    for (i, mr) in results.iter().enumerate() {
        println!(
            "│ {:<8} │ {:>10} │ {:>12} │ {:>12} │",
            format!("{}×", mr.mux),
            mr.conns,
            cell(mr.r.p50(), i == wi_p50),
            cell(mr.r.p99(), i == wi_p99),
        );
    }
    println!(
        "└{0}┴{1}┴{2}┴{2}┘",
        "─".repeat(10),
        "─".repeat(12),
        "─".repeat(14),
    );

    let baseline_p50 = results[0].r.p50();
    let best_p50 = results[wi_p50].r.p50();
    if baseline_p50 > 0 && wi_p50 != 0 {
        println!(
            "\n  Best mux={} ({} conns): {:.1}× lower P50 than mux=1 ({} conns)",
            results[wi_p50].mux,
            results[wi_p50].conns,
            baseline_p50 as f64 / best_p50 as f64,
            results[0].conns,
        );
    }
    println!("  ↓ less is better   ★ = winner\n");
}

// ── REST SSE fanout ───────────────────────────────────────────────────────────

async fn bench_fanout_rest(watchers: usize, rounds: usize) -> FanoutResult {
    let (report_tx, mut report_rx) = tokio::sync::mpsc::unbounded_channel::<(Instant, usize)>();

    for _ in 0..watchers {
        let tx = report_tx.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let resp = client
                .get("http://127.0.0.1:7778/state/fanout.rest/events")
                .send()
                .await
                .unwrap();
            let mut stream = resp.bytes_stream();
            let mut buf = Vec::<u8>::new();

            while let Some(Ok(chunk)) = stream.next().await {
                buf.extend_from_slice(&chunk);
                // A complete SSE event ends with \n\n; emit one report per event.
                while let Some(pos) = find_double_newline(&buf) {
                    let event_len = pos + 2;
                    let _ = tx.send((Instant::now(), event_len));
                    buf.drain(..event_len);
                }
            }
        });
    }
    drop(report_tx);
    sleep(Duration::from_millis(600)).await;

    let client = reqwest::Client::new();
    let mut max_latencies_us = Vec::with_capacity(rounds);
    let mut total_bytes_per_round = Vec::with_capacity(rounds);

    for r in 0..rounds as u64 {
        let body = serde_json::to_vec(&test_object(r)).unwrap();

        let send_time = Instant::now();
        client
            .post("http://127.0.0.1:7778/state/fanout.rest")
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();

        let (max_us, total_bytes) = collect_receipts(&mut report_rx, watchers, send_time).await;
        max_latencies_us.push(max_us);
        total_bytes_per_round.push(total_bytes);
    }

    FanoutResult {
        name: "REST (SSE)".into(),
        max_latencies_us,
        total_bytes_per_round,
    }
}

// ── gRPC fanout ───────────────────────────────────────────────────────────────

async fn bench_fanout_grpc(watchers: usize, rounds: usize) -> FanoutResult {
    let (report_tx, mut report_rx) = tokio::sync::mpsc::unbounded_channel::<(Instant, usize)>();

    // Cloning a tonic client reuses the same HTTP/2 connection (multiplexed).
    let base = StateServiceClient::connect("http://127.0.0.1:7779")
        .await
        .unwrap();

    for _ in 0..watchers {
        let mut client = base.clone();
        let tx = report_tx.clone();
        tokio::spawn(async move {
            let mut stream = client
                .watch_state(WatchRequest {
                    key: "fanout.grpc".into(),
                })
                .await
                .unwrap()
                .into_inner();

            while let Some(Ok(event)) = stream.next().await {
                let bytes = event.encode_to_vec().len();
                let _ = tx.send((Instant::now(), bytes));
            }
        });
    }
    drop(report_tx);
    sleep(Duration::from_millis(600)).await;

    let mut writer = base.clone();
    let mut max_latencies_us = Vec::with_capacity(rounds);
    let mut total_bytes_per_round = Vec::with_capacity(rounds);

    for r in 0..rounds as u64 {
        let req = SetStateRequest {
            key: "fanout.grpc".into(),
            value: serde_json::to_vec(&test_object(r)).unwrap(),
        };

        let send_time = Instant::now();
        writer.set_state(req).await.unwrap();

        let (max_us, total_bytes) = collect_receipts(&mut report_rx, watchers, send_time).await;
        max_latencies_us.push(max_us);
        total_bytes_per_round.push(total_bytes);
    }

    FanoutResult {
        name: "gRPC (stream)".into(),
        max_latencies_us,
        total_bytes_per_round,
    }
}

// ── fanout output ─────────────────────────────────────────────────────────────

fn print_fanout_results(results: &[FanoutResult], watchers: usize) {
    let p50s: Vec<u128> = results.iter().map(|r| r.p50()).collect();
    let p99s: Vec<u128> = results.iter().map(|r| r.p99()).collect();
    let bytes: Vec<usize> = results.iter().map(|r| r.avg_total_bytes()).collect();

    let wi_p50 = winner_min(&p50s);
    let wi_p99 = winner_min(&p99s);
    let wi_bytes = winner_min_usize(&bytes);

    println!("\n── Fanout: {watchers} simultaneous watchers, 1 writer ──────────────────────────");
    println!("   Latency = time until the LAST of the {watchers} watchers receives the update\n");

    println!(
        "┌{0}┬{1}┬{1}┬{2}┐",
        "─".repeat(18),
        "─".repeat(14),
        "─".repeat(22),
    );
    println!(
        "│ {:<16} │ {:>12} │ {:>12} │ {:>20} │",
        "Technology", "P50 µs (↓)", "P99 µs (↓)", "Total bytes/round (↓)",
    );
    println!(
        "├{0}┼{1}┼{1}┼{2}┤",
        "─".repeat(18),
        "─".repeat(14),
        "─".repeat(22),
    );
    for (i, r) in results.iter().enumerate() {
        println!(
            "│ {:<16} │ {:>12} │ {:>12} │ {:>20} │",
            r.name,
            cell(r.p50(), i == wi_p50),
            cell(r.p99(), i == wi_p99),
            cell(r.avg_total_bytes(), i == wi_bytes),
        );
    }
    println!(
        "└{0}┴{1}┴{1}┴{2}┘",
        "─".repeat(18),
        "─".repeat(14),
        "─".repeat(22),
    );

    println!("\n  ↓ less is better   ★ = winner\n");
    println!(
        "  P50 tail-until-all (↓ less is better)  →  {} {} µs",
        results[wi_p50].name, p50s[wi_p50]
    );
    println!(
        "  P99 tail-until-all (↓ less is better)  →  {} {} µs",
        results[wi_p99].name, p99s[wi_p99]
    );

    let per_w = results[wi_bytes].avg_total_bytes() / watchers;
    let runner_up_bytes = bytes
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != wi_bytes)
        .map(|(_, &b)| b)
        .min()
        .unwrap_or(0);
    println!(
        "  Total bytes/round  (↓ less is better)  →  {} {} B  ({watchers} × {per_w} B per watcher, {:.1}× less than runner-up)",
        results[wi_bytes].name,
        bytes[wi_bytes],
        runner_up_bytes as f64 / bytes[wi_bytes] as f64
    );

    println!(
        "\n  At 1 000 watchers SODP would save ≈{} KB per mutation vs REST.",
        (bytes[1].saturating_sub(bytes[0])) * 10 / 1024
    );
}

// ─── RESUME smoke test ────────────────────────────────────────────────────────
//
// Verifies that a reconnecting client receives exactly the deltas it missed.
//
// Timeline:
//   write v_a, v_b, v_c  (watcher catches all three via WATCH)
//   write v_d, v_e        (watcher "disconnects" before these arrive)
//   reconnect with RESUME { since_version: v_c }
//   → expect 2 DELTA frames (v_d, v_e) then STATE_INIT at v_e

async fn test_resume() {
    use sodp::frame::Frame;

    // ── helper: send CALL and return the version from RESULT ──────────────────
    async fn set_state(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        seq: u64,
        value: u64,
    ) -> u64 {
        let call = Frame {
            frame_type: types::CALL,
            stream_id: 0,
            seq,
            body: serde_json::json!({
                "call_id": seq.to_string(),
                "method":  "state.set",
                "args":    { "state": "test.resume", "value": { "n": value } },
            }),
        };
        ws.send(WsMsg::Binary(call.encode().unwrap()))
            .await
            .unwrap();
        // Read RESULT — body is { call_id, success, data: { version: N } }
        if let Some(Ok(WsMsg::Binary(bytes))) = ws.next().await
            && let Ok(f) = Frame::decode(&bytes)
        {
            return f
                .body
                .get("data")
                .and_then(|d| d.get("version"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        }
        0
    }

    // ── writer connection ─────────────────────────────────────────────────────
    let (mut writer, _) = connect_async("ws://127.0.0.1:7777")
        .await
        .expect("resume: writer connect");
    writer.next().await; // HELLO

    // ── watcher that will later "disconnect" ──────────────────────────────────
    let (mut watcher, _) = connect_async("ws://127.0.0.1:7777")
        .await
        .expect("resume: watcher connect");
    watcher.next().await; // HELLO
    let watch = Frame {
        frame_type: types::WATCH,
        stream_id: 0,
        seq: 0,
        body: serde_json::json!({ "state": "test.resume" }),
    };
    watcher
        .send(WsMsg::Binary(watch.encode().unwrap()))
        .await
        .unwrap();
    watcher.next().await; // STATE_INIT

    // Write three values; watcher receives all three.
    let _v_a = set_state(&mut writer, 1, 1).await;
    watcher.next().await; // DELTA v_a
    let _v_b = set_state(&mut writer, 2, 2).await;
    watcher.next().await; // DELTA v_b
    let v_c = set_state(&mut writer, 3, 3).await;
    watcher.next().await; // DELTA v_c  ← watcher last sees v_c

    // Simulate disconnect: drop the watcher.
    drop(watcher);
    sleep(Duration::from_millis(30)).await;

    // Write two more values while client is offline.
    let _v_d = set_state(&mut writer, 4, 4).await;
    let v_e = set_state(&mut writer, 5, 5).await;

    // ── reconnect and RESUME ──────────────────────────────────────────────────
    let (mut resumed, _) = connect_async("ws://127.0.0.1:7777")
        .await
        .expect("resume: reconnect");
    resumed.next().await; // HELLO

    let resume_frame = Frame {
        frame_type: types::RESUME,
        stream_id: 0,
        seq: 0,
        body: serde_json::json!({ "state": "test.resume", "since_version": v_c }),
    };
    resumed
        .send(WsMsg::Binary(resume_frame.encode().unwrap()))
        .await
        .unwrap();

    // Collect all frames until STATE_INIT (the "you are now live" marker).
    let mut replayed_versions: Vec<u64> = Vec::new();
    let mut final_version = 0u64;

    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_secs(2), resumed.next()).await {
            Ok(Some(Ok(WsMsg::Binary(bytes)))) => {
                let f = Frame::decode(&bytes).unwrap();
                match f.frame_type {
                    types::DELTA => {
                        let v = f.body.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                        replayed_versions.push(v);
                    }
                    types::STATE_INIT => {
                        final_version = f.body.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                        break;
                    }
                    _ => {}
                }
            }
            _ => break,
        }
    }

    let ok = replayed_versions.len() == 2 && final_version == v_e;

    println!("\n── RESUME smoke test ────────────────────────────────────────────────────────");
    println!("  Client last saw v{v_c}, missed v_d and v_e (v{v_e})");
    println!(
        "  Replayed {} DELTA(s): {:?}",
        replayed_versions.len(),
        replayed_versions
    );
    println!(
        "  STATE_INIT at v{final_version}  →  {}",
        if ok { "PASS ✓" } else { "FAIL ✗" }
    );
}

// ─── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // silence server log noise during benchmark
    tracing_subscriber::fmt().with_env_filter("error").init();

    // BENCH_QUICK=1  → fewer iterations, no fanout; used for profiling runs.
    // BENCH_SCALE=1  → skip single-client + fanout; run only the scalability sweep.
    let quick = std::env::var("BENCH_QUICK").is_ok();
    let scale_only = std::env::var("BENCH_SCALE").is_ok();
    let warmup = if quick { 50 } else { 500 };
    let iters = if quick { 300 } else { 2_000 };
    let fanout_watchers = 100usize;
    let fanout_rounds = if quick || scale_only { 0 } else { 300 };

    println!("Starting servers...");
    tokio::spawn(async {
        SodpServer::new()
            .listen("127.0.0.1:7777", tokio_util::sync::CancellationToken::new())
            .await
            .unwrap()
    });
    tokio::spawn(start_rest_server("127.0.0.1:7778"));
    tokio::spawn(start_grpc_server("127.0.0.1:7779"));
    sleep(Duration::from_millis(300)).await;

    if !scale_only {
        // ── single-client latency / byte efficiency ───────────────────────────
        println!("Warming up ({warmup} iterations each)...");
        bench_sodp(warmup).await;
        bench_rest(warmup).await;
        bench_grpc(warmup).await;

        println!("Benchmarking single-client ({iters} iterations each)...");
        let results = vec![
            bench_sodp(iters).await,
            bench_rest(iters).await,
            bench_grpc(iters).await,
        ];
        println!("\n── Single client ────────────────────────────────────────────────────────────");
        print_results(&results);
    } else {
        // Brief server warmup even in scale-only mode.
        println!("Warming up...");
        bench_sodp(100).await;
    }

    // ── SODP scalability sweep ────────────────────────────────────────────────
    // Shows how fanout overhead scales with subscriber count, and the benefit
    // of multiplexing many subscriptions onto fewer physical connections.
    let mux = 100usize;
    // scale_only uses full watcher counts + higher rounds; quick uses smaller set.
    let nomux_counts: &[usize] = if quick && !scale_only {
        &[0, 10, 100, 1000]
    } else {
        &[0, 10, 100, 500, 1000]
    };
    let mux_counts: &[usize] = if quick && !scale_only {
        &[0, 100, 1000]
    } else {
        &[0, 100, 500, 1000]
    };

    const RUST_URL: &str = "ws://127.0.0.1:7777";
    const GO_URL: &str = "ws://127.0.0.1:7780";
    let full = !quick || scale_only;

    println!("\nScale sweep: mux=1...");
    let nomux_rows = bench_scale_sweep(RUST_URL, nomux_counts, 1, !full).await;
    println!("Scale sweep: mux={mux}...");
    let mux_rows = bench_scale_sweep(RUST_URL, mux_counts, mux, !full).await;

    println!("\n── SODP scalability ────────────────────────────────────────────────────────");
    println!("   ops/sec = mutations/s sustained while waiting for all watchers to receive");
    print_scale_table("Without mux (1 connection per watcher):", &nomux_rows);
    print_scale_table(
        &format!("With mux={mux} ({mux} subscriptions per connection):"),
        &mux_rows,
    );

    // ── Rust vs Go comparison ─────────────────────────────────────────────────
    if std::env::var("BENCH_COMPARE").is_ok() {
        println!("\n── Rust vs Go comparison ───────────────────────────────────────────────────");
        println!("   Starting sodp-go server on {GO_URL}...");
        let mut go_proc = std::process::Command::new("go")
            .args([
                "run",
                "./cmd/sodp-server",
                "-addr",
                "127.0.0.1:7780",
                "-rate",
                "1000000",
                "-backpressure",
                "4096",
            ])
            .current_dir("sodp-go")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to start sodp-go (is Go installed?)");
        // go run compiles then starts — allow enough time.
        sleep(Duration::from_secs(8)).await;

        println!("   Go server ready. Running sweeps...");
        println!("   mux=1...");
        let go_nomux = bench_scale_sweep(GO_URL, nomux_counts, 1, !full).await;
        println!("   mux={mux}...");
        let go_mux = bench_scale_sweep(GO_URL, mux_counts, mux, !full).await;

        go_proc.kill().ok();

        print_scale_comparison("mux=1 (1 conn per watcher)", &nomux_rows, &go_nomux);
        print_scale_comparison(
            &format!("mux={mux} ({mux} subs per conn)"),
            &mux_rows,
            &go_mux,
        );
    }

    // ── fanout (3-way comparison, mux=1 baseline) ────────────────────────────
    if fanout_rounds > 0 {
        println!("\nFanout warmup ({fanout_watchers} watchers, 50 rounds each)...");
        bench_fanout_sodp(fanout_watchers, 1, 50).await;
        bench_fanout_rest(fanout_watchers, 50).await;
        bench_fanout_grpc(fanout_watchers, 50).await;

        println!("Fanout benchmark ({fanout_watchers} watchers, {fanout_rounds} rounds each)...");
        let fanout_results = vec![
            bench_fanout_sodp(fanout_watchers, 1, fanout_rounds).await,
            bench_fanout_rest(fanout_watchers, fanout_rounds).await,
            bench_fanout_grpc(fanout_watchers, fanout_rounds).await,
        ];
        print_fanout_results(&fanout_results, fanout_watchers);
    }

    if !scale_only {
        // ── SODP mux matrix ───────────────────────────────────────────────────
        let mux_watchers = if quick { fanout_watchers } else { 500 };
        let mux_rounds = if quick { 50 } else { fanout_rounds };
        println!("\nMux matrix ({mux_watchers} watchers, {mux_rounds} rounds each)...");
        let mux_results = bench_mux_matrix(mux_watchers, mux_rounds).await;
        print_mux_matrix(&mux_results, mux_watchers, mux_rounds);

        // ── RESUME correctness verification ───────────────────────────────────
        test_resume().await;
    }
}
