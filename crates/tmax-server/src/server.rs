use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tracing::{error, info};

use libtmax::session::SessionManager;

use crate::config::ServerConfig;
use crate::connection;

pub type SharedState = Arc<Mutex<SessionManager>>;

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    // Clean up stale socket
    if config.socket_path.exists() {
        std::fs::remove_file(&config.socket_path)?;
    }

    // Ensure parent directory exists
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write PID file
    let pid_path = ServerConfig::pid_file_path();
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&pid_path, std::process::id().to_string())?;

    let listener = UnixListener::bind(&config.socket_path)?;
    info!(socket = %config.socket_path.display(), pid = std::process::id(), "tmax server started");

    let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));

    // Handle shutdown signals
    let socket_path = config.socket_path.clone();
    let pid_path_clone = pid_path.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("shutting down...");
        // Cleanup
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&pid_path_clone);
        std::process::exit(0);
    });

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    connection::handle_client(stream, state).await;
                });
            }
            Err(e) => {
                error!("accept error: {e}");
            }
        }
    }
}
