use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use libtmax::session::SessionManager;

use crate::config::ServerConfig;
use crate::connection;

pub type SharedState = Arc<Mutex<SessionManager>>;

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    // Clean up stale socket, verifying it is actually a socket
    if config.socket_path.exists() {
        let metadata = std::fs::symlink_metadata(&config.socket_path)?;
        if metadata.file_type().is_socket() {
            std::fs::remove_file(&config.socket_path)?;
        } else {
            anyhow::bail!(
                "socket path {} exists but is not a socket -- refusing to remove",
                config.socket_path.display()
            );
        }
    }

    // Ensure parent directory exists with owner-only permissions
    if let Some(parent) = config.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }

    // Write PID file
    let pid_path = ServerConfig::pid_file_path();
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&pid_path, std::process::id().to_string())?;

    let listener = UnixListener::bind(&config.socket_path)?;
    std::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o700))?;
    info!(socket = %config.socket_path.display(), pid = std::process::id(), "tmax server started");

    let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));

    let shutdown = CancellationToken::new();

    // Handle shutdown signals
    let shutdown_clone = shutdown.clone();
    let socket_path = config.socket_path.clone();
    let pid_path_clone = pid_path.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("shutting down...");
        // Cleanup files
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&pid_path_clone);
        // Signal shutdown
        shutdown_clone.cancel();
    });

    // Accept loop with graceful shutdown
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
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
            _ = shutdown.cancelled() => {
                info!("server stopped");
                break;
            }
        }
    }

    Ok(())
}
