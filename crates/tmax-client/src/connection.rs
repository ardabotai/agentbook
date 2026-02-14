use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tmax_protocol::{Request, Response};

/// Async connection to the tmax server with split read/write halves
/// for concurrent use in tokio::select!.
pub struct ServerConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl ServerConnection {
    /// Connect to the tmax server via Unix socket.
    pub async fn connect() -> anyhow::Result<Self> {
        let socket_path = tmax_protocol::paths::default_socket_path();

        let stream = UnixStream::connect(&socket_path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::ConnectionRefused
                || e.kind() == std::io::ErrorKind::NotFound
            {
                anyhow::anyhow!(
                    "tmax server is not running. Start it with: tmax server start"
                )
            } else {
                anyhow::anyhow!(
                    "failed to connect to tmax server at {}: {e}",
                    socket_path.display()
                )
            }
        })?;

        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            reader: BufReader::new(read_half),
            writer: write_half,
        })
    }

    /// Send a request and read the response.
    pub async fn send_request(&mut self, req: &Request) -> anyhow::Result<Response> {
        let json = serde_json::to_string(req)?;
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;

        let mut line = String::new();
        self.reader.read_line(&mut line).await?;

        if line.is_empty() {
            anyhow::bail!("server closed connection");
        }

        let response: Response = serde_json::from_str(&line)?;
        Ok(response)
    }

    /// Read the next event/response from the server (for streaming).
    pub async fn read_event(&mut self) -> anyhow::Result<Option<Response>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let response: Response = serde_json::from_str(&line)?;
        Ok(Some(response))
    }

}
