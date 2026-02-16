use crate::protocol::{MAX_LINE_BYTES, Request, Response};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use tokio::net::UnixStream;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

/// Client for the agentbook node daemon's Unix socket API.
pub struct NodeClient {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
    writer: FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
    node_id: String,
}

impl NodeClient {
    /// Connect to the node daemon at the given socket path.
    /// Waits for the Hello response before returning.
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("failed to connect to {}", socket_path.display()))?;
        let (r, w) = stream.into_split();
        let reader = FramedRead::new(r, LinesCodec::new_with_max_length(MAX_LINE_BYTES));
        let writer = FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_LINE_BYTES));

        let mut client = Self {
            reader,
            writer,
            node_id: String::new(),
        };

        match client.next_response().await? {
            Response::Hello { node_id, .. } => {
                client.node_id = node_id;
                Ok(client)
            }
            other => Err(anyhow!("expected Hello, got {other:?}")),
        }
    }

    /// The node ID received from the Hello handshake.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Send a request to the daemon.
    pub async fn send(&mut self, req: Request) -> Result<()> {
        let line = serde_json::to_string(&req)?;
        self.writer.send(line).await?;
        Ok(())
    }

    /// Read the next response from the daemon.
    pub async fn next_response(&mut self) -> Result<Response> {
        let Some(line) = self.reader.next().await else {
            bail!("daemon disconnected");
        };
        Ok(serde_json::from_str(&line?)?)
    }

    /// Send a request and wait for the Ok/Error response, skipping events.
    pub async fn request(&mut self, req: Request) -> Result<Option<serde_json::Value>> {
        self.send(req).await?;
        loop {
            match self.next_response().await? {
                Response::Hello { .. } | Response::Event { .. } => continue,
                Response::Ok { data } => return Ok(data),
                Response::Error { message, .. } => bail!("{message}"),
            }
        }
    }
}

/// Discover the default socket path.
///
/// Checks `$AGENTBOOK_SOCKET` env, then falls back to
/// `$XDG_RUNTIME_DIR/agentbook/agentbook.sock` or `/tmp/agentbook-$UID/agentbook.sock`.
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("AGENTBOOK_SOCKET") {
        return PathBuf::from(p);
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir)
            .join("agentbook")
            .join("agentbook.sock");
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/agentbook-{uid}/agentbook.sock"))
}
