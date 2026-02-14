mod api;
mod client;
mod ws;
mod ws_protocol;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use axum::Router;
use clap::Parser;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use ws::AppState;

#[derive(Parser, Debug)]
#[command(name = "tmax-web", about = "WebSocket bridge for tmax terminal multiplexer")]
struct Args {
    /// HTTP listen address.
    #[arg(long, default_value = "127.0.0.1:7860")]
    listen: String,

    /// Path to the tmax-server Unix socket.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Output frame batch interval in milliseconds.
    #[arg(long, default_value = "16")]
    batch_interval_ms: u64,

    /// Maximum lag chunks before skipping to latest.
    #[arg(long, default_value = "1000")]
    max_lag_chunks: u64,

    /// Allowed CORS origins (comma-separated). Use '*' for any.
    #[arg(long, default_value = "http://localhost:3000")]
    cors_origins: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tmax_web=info".into()),
        )
        .init();

    let args = Args::parse();

    let socket_path = args
        .socket
        .unwrap_or_else(tmax_protocol::paths::default_socket_path);

    let state = Arc::new(AppState {
        socket_path,
        batch_interval: Duration::from_millis(args.batch_interval_ms),
        max_lag_chunks: args.max_lag_chunks,
    });

    // CORS layer
    let cors = if args.cors_origins == "*" {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        let origins: Vec<_> = args
            .cors_origins
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any)
    };

    let app = Router::new()
        // REST API
        .route("/api/sessions", get(api::list_sessions))
        .route("/api/sessions/tree", get(api::session_tree))
        .route("/api/sessions/{id}", get(api::session_info))
        // WebSocket
        .route("/ws", get(ws::ws_handler))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    info!(listen = %args.listen, "tmax-web started");

    axum::serve(listener, app).await?;

    Ok(())
}
