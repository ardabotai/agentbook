use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::debug;

use tmax_protocol::{Request, Response};

/// Client that communicates with tmax-server over a Unix socket.
/// Each instance holds a single persistent connection.
pub struct TmaxClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl TmaxClient {
    /// Connect to the tmax-server at the given socket path.
    pub async fn connect(socket_path: &Path) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read_half),
            writer: write_half,
        })
    }

    /// Send a request and wait for a single response.
    pub async fn request(&mut self, req: &Request) -> anyhow::Result<Response> {
        let json = serde_json::to_string(req)?;
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;

        let mut line = String::new();
        self.reader.read_line(&mut line).await?;
        if line.is_empty() {
            anyhow::bail!("server closed connection");
        }
        let resp: Response = serde_json::from_str(line.trim())?;
        Ok(resp)
    }

    /// Send a request without waiting for the response.
    /// Used when we want to fire-and-forget (e.g., before reading events).
    #[allow(dead_code)]
    pub async fn send(&mut self, req: &Request) -> anyhow::Result<()> {
        let json = serde_json::to_string(req)?;
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Read the next line from the server (response or event).
    pub async fn read_line(&mut self) -> anyhow::Result<Option<String>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(line))
    }

    /// Read and parse the next response from the server.
    #[allow(dead_code)]
    pub async fn read_response(&mut self) -> anyhow::Result<Option<Response>> {
        match self.read_line().await? {
            Some(line) => {
                let resp: Response = serde_json::from_str(line.trim())?;
                Ok(Some(resp))
            }
            None => Ok(None),
        }
    }
}

/// Create a new client connection using the default or specified socket path.
pub async fn connect(socket_path: Option<&Path>) -> anyhow::Result<TmaxClient> {
    let path = match socket_path {
        Some(p) => p.to_path_buf(),
        None => tmax_protocol::paths::default_socket_path(),
    };
    debug!(socket = %path.display(), "connecting to tmax-server");
    TmaxClient::connect(&path).await
}
