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

    /// Split into independent reader and writer halves.
    ///
    /// Use this when you need to poll for events in a `select!` loop while
    /// still sending requests. The reader yields all responses (including
    /// events); the writer sends requests.
    pub fn into_split(self) -> (NodeWriter, NodeReader) {
        (
            NodeWriter {
                writer: self.writer,
                node_id: self.node_id,
            },
            NodeReader {
                reader: self.reader,
            },
        )
    }
}

/// Write half of a split [`NodeClient`]. Sends requests to the daemon.
pub struct NodeWriter {
    writer: FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
    node_id: String,
}

impl NodeWriter {
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub async fn send(&mut self, req: Request) -> Result<()> {
        let line = serde_json::to_string(&req)?;
        self.writer.send(line).await?;
        Ok(())
    }
}

/// Read half of a split [`NodeClient`]. Yields all responses including events.
pub struct NodeReader {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
}

impl NodeReader {
    /// Read the next response/event from the daemon.
    /// Returns `None` if the daemon disconnected.
    pub async fn next(&mut self) -> Option<Result<Response>> {
        let line = self.reader.next().await?;
        Some(
            line.map_err(Into::into)
                .and_then(|l| serde_json::from_str(&l).map_err(Into::into)),
        )
    }
}

/// Lightweight client for the agentbook-agent credential vault.
pub struct AgentClient {
    stream: tokio::net::UnixStream,
}

impl AgentClient {
    /// Connect to the agent socket. Returns `None` if the socket doesn't exist.
    pub async fn connect(socket_path: &Path) -> Option<Self> {
        let stream = tokio::net::UnixStream::connect(socket_path).await.ok()?;
        Some(Self { stream })
    }

    /// Ask the agent for the KEK. Returns `None` if locked or agent unreachable.
    pub async fn get_kek(&mut self) -> Option<zeroize::Zeroizing<[u8; 32]>> {
        use crate::agent_protocol::{AgentRequest, AgentResponse};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let req = serde_json::to_string(&AgentRequest::GetKek).ok()?;
        self.stream
            .write_all(format!("{req}\n").as_bytes())
            .await
            .ok()?;

        let (read, _) = self.stream.split();
        let mut lines = BufReader::new(read).lines();
        let line = lines.next_line().await.ok()??;

        let resp: AgentResponse = serde_json::from_str(&line).ok()?;
        match resp {
            AgentResponse::Kek { kek_b64 } => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&kek_b64)
                    .ok()?;
                let arr: [u8; 32] = bytes.try_into().ok()?;
                Some(zeroize::Zeroizing::new(arr))
            }
            _ => None,
        }
    }

    /// Send a request that expects an `Ok` or `Error` response.
    pub async fn request_ok(&mut self, req: &crate::agent_protocol::AgentRequest) -> Result<()> {
        use crate::agent_protocol::AgentResponse;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let req_json = serde_json::to_string(req)?;
        self.stream
            .write_all(format!("{req_json}\n").as_bytes())
            .await?;

        let (read, _) = self.stream.split();
        let mut lines = BufReader::new(read).lines();
        let line = lines
            .next_line()
            .await?
            .context("agent closed connection")?;

        match serde_json::from_str::<AgentResponse>(&line)? {
            AgentResponse::Ok | AgentResponse::Status { .. } => Ok(()),
            AgentResponse::Error { message } => anyhow::bail!("{message}"),
            AgentResponse::Kek { .. } => Ok(()),
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
