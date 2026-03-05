#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

use metrics_exporter_prometheus::PrometheusHandle;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use sodp::cluster::RedisCluster;
use sodp::server::SodpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let addr        = std::env::args().nth(1).unwrap_or_else(|| "0.0.0.0:7777".to_string());
    // Empty string treated as absent so `sodp-server addr "" schema.json` works.
    let log_dir     = std::env::args().nth(2).filter(|s| !s.is_empty());
    let schema_file = std::env::args().nth(3).filter(|s| !s.is_empty());

    let mut server = match schema_file {
        Some(sf) => SodpServer::with_schema(
            log_dir.as_deref().map(std::path::Path::new),
            std::path::Path::new(&sf),
        )?,
        None => match log_dir {
            Some(ref dir) => SodpServer::new_persistent(std::path::Path::new(dir))?,
            None          => SodpServer::new(),
        },
    };

    let shutdown = CancellationToken::new();

    // Redis horizontal scaling (SODP_REDIS_URL).
    // Connect before spawning any tasks so Arc::try_unwrap succeeds.
    if let Ok(url) = std::env::var("SODP_REDIS_URL") {
        match RedisCluster::connect(&url).await {
            Ok(cluster) => {
                match cluster.load_state().await {
                    Ok(entries) => {
                        // Safe: no tasks spawned yet — only one Arc reference exists.
                        let mut inner = Arc::try_unwrap(server).unwrap_or_else(|_| {
                            unreachable!("no tasks spawned yet — Arc must have exactly one reference")
                        });
                        inner.state.load_entries(entries);
                        cluster.clone().spawn_subscriber(
                            Arc::clone(&inner.fanout),
                            shutdown.clone(),
                        );
                        inner.cluster = Some(cluster);
                        server = Arc::new(inner);
                    }
                    Err(e) => error!("Redis: failed to load state: {e} — continuing without Redis"),
                }
            }
            Err(e) => error!("Redis: failed to connect to '{url}': {e} — continuing without Redis"),
        }
    }

    // Health check server (SODP_HEALTH_PORT, e.g. 7778)
    if let Ok(port_str) = std::env::var("SODP_HEALTH_PORT")
        && let Ok(port) = port_str.parse::<u16>() {
            let connections = Arc::clone(&server.connections);
            let token = shutdown.clone();
            tokio::spawn(run_health_server(port, connections, token));
        }

    // Prometheus metrics server (SODP_METRICS_PORT, e.g. 9090).
    // If unset, the metrics crate uses its built-in no-op recorder — zero overhead.
    if let Ok(port_str) = std::env::var("SODP_METRICS_PORT")
        && let Ok(port) = port_str.parse::<u16>() {
            match metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder() {
                Ok(handle) => {
                    info!("Prometheus metrics on 0.0.0.0:{port}");
                    tokio::spawn(run_metrics_server(port, handle, shutdown.clone()));
                }
                Err(e) => error!("Failed to install Prometheus recorder: {e}"),
            }
        }

    // Graceful shutdown on Ctrl-C or SIGTERM
    let token = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm() => {}
        }
        info!("Shutdown signal received");
        token.cancel();
    });

    server.listen(&addr, shutdown).await
}

/// Returns when SIGTERM is received (Unix) or never (non-Unix).
async fn sigterm() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut stream = signal(SignalKind::terminate()).expect("SIGTERM handler");
        stream.recv().await;
    }
    #[cfg(not(unix))]
    std::future::pending::<()>().await;
}

/// Minimal HTTP health server.
///
/// Every accepted TCP connection receives a static `200 OK` JSON response.
/// `SODP_HEALTH_PORT` controls the port; curl http://host:PORT/health works.
async fn run_health_server(port: u16, connections: Arc<AtomicUsize>, shutdown: CancellationToken) {
    let addr = format!("0.0.0.0:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l)  => { info!("Health server listening on {addr}"); l }
        Err(e) => { error!("Failed to bind health server on {addr}: {e}"); return; }
    };

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((mut stream, _)) => {
                        let n    = connections.load(Ordering::Relaxed);
                        let body = format!(r#"{{"status":"ok","connections":{n},"version":"0.1"}}"#);
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(), body
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                    }
                    Err(e) => error!("Health accept error: {e}"),
                }
            }
            _ = shutdown.cancelled() => break,
        }
    }
}

/// Prometheus metrics server.
///
/// Every accepted TCP connection receives the full Prometheus text exposition
/// format produced by `handle.render()`.  Compatible with `prometheus`,
/// `victoria-metrics`, and any Prometheus-compatible scraper.
///
/// Content-Type `text/plain; version=0.0.4` is required by the Prometheus
/// data model specification.
async fn run_metrics_server(port: u16, handle: PrometheusHandle, shutdown: CancellationToken) {
    let addr = format!("0.0.0.0:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l)  => l,
        Err(e) => { error!("Failed to bind metrics server on {addr}: {e}"); return; }
    };

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((mut stream, _)) => {
                        let body = handle.render();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(), body
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                    }
                    Err(e) => error!("Metrics accept error: {e}"),
                }
            }
            _ = shutdown.cancelled() => break,
        }
    }
}
