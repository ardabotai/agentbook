use crate::handler::{NodeState, handle_request};
use agentbook::protocol::{MAX_LINE_BYTES, Request, Response};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

/// Start the Unix socket server. Accepts client connections and processes requests.
pub async fn serve(state: Arc<NodeState>, socket_path: &Path) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).ok();
        }
    }

    // Remove stale socket
    if socket_path.exists() {
        std::fs::remove_file(socket_path).ok();
    }

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)).ok();
    }

    tracing::info!(path = %socket_path.display(), "Unix socket listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(state, stream).await {
                tracing::debug!(err = %e, "client disconnected");
            }
        });
    }
}

async fn handle_client(state: Arc<NodeState>, stream: tokio::net::UnixStream) -> Result<()> {
    let (r, w) = stream.into_split();
    let mut reader = FramedRead::new(r, LinesCodec::new_with_max_length(MAX_LINE_BYTES));
    let mut writer = FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_LINE_BYTES));

    // Send Hello
    let hello = Response::Hello {
        node_id: state.identity.node_id.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let hello_line = serde_json::to_string(&hello)?;
    writer.send(hello_line).await?;

    // Subscribe to events
    let mut event_rx = state.event_tx.subscribe();

    loop {
        tokio::select! {
            line = reader.next() => {
                let Some(line) = line else { break };
                let line = line?;
                let req: Request = serde_json::from_str(&line)
                    .with_context(|| format!("invalid request: {line}"))?;

                let is_shutdown = matches!(req, Request::Shutdown);
                let resp = handle_request(&state, req).await;
                let resp_line = serde_json::to_string(&resp)?;
                writer.send(resp_line).await?;

                if is_shutdown {
                    break;
                }
            }
            event = event_rx.recv() => {
                if let Ok(event) = event {
                    let resp = Response::Event { event };
                    let resp_line = serde_json::to_string(&resp)?;
                    writer.send(resp_line).await?;
                }
            }
        }
    }

    Ok(())
}
