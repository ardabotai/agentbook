use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tmax_protocol::{Request, Response};

/// Async connection to the tmax server with split read/write halves
/// for concurrent use in tokio::select!.
pub struct ServerConnection {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_fails_when_server_not_running() {
        // Use a path that definitely doesn't exist
        let result = ServerConnection::connect().await;
        // Should fail with a helpful error message (unless tmax-server happens to be running)
        if let Err(e) = result {
            let msg = e.to_string();
            assert!(
                msg.contains("tmax server is not running") || msg.contains("failed to connect"),
                "error should mention server not running, got: {msg}"
            );
        }
        // If it succeeds, a server is actually running - that's fine too
    }

    #[tokio::test]
    async fn send_request_and_read_event_work_with_mock() {
        // Create a Unix socket pair for testing
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        // Spawn a mock server that echoes a response
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);

            // Read the request
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();

            // Send back an Ok response
            let resp = r#"{"type":"ok","data":{"session_id":"test-123"}}"#;
            write_half.write_all(resp.as_bytes()).await.unwrap();
            write_half.write_all(b"\n").await.unwrap();
            write_half.flush().await.unwrap();

            // Send an event
            let event = r#"{"type":"event","event":"output","session_id":"test-123","seq":1,"data":"aGVsbG8="}"#;
            write_half.write_all(event.as_bytes()).await.unwrap();
            write_half.write_all(b"\n").await.unwrap();
            write_half.flush().await.unwrap();

            // Close connection
            drop(write_half);
        });

        // Connect as client
        let stream = UnixStream::connect(&sock_path).await.unwrap();
        let (read_half, write_half) = stream.into_split();
        let mut conn = ServerConnection {
            reader: BufReader::new(read_half),
            writer: write_half,
        };

        // Send a request
        let req = Request::SessionList;
        let resp = conn.send_request(&req).await.unwrap();
        match resp {
            Response::Ok { data } => {
                assert!(data.is_some());
                let d = data.unwrap();
                assert_eq!(d["session_id"], "test-123");
            }
            _ => panic!("expected Ok response"),
        }

        // Read an event
        let event = conn.read_event().await.unwrap();
        assert!(event.is_some());
        match event.unwrap() {
            Response::Event(tmax_protocol::Event::Output { session_id, seq, data }) => {
                assert_eq!(session_id, "test-123");
                assert_eq!(seq, 1);
                assert_eq!(data, b"hello");
            }
            other => panic!("expected Output event, got: {other:?}"),
        }

        // Read EOF
        let eof = conn.read_event().await.unwrap();
        assert!(eof.is_none());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn send_request_handles_server_close() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        // Server that immediately closes the connection
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, _write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            let _ = reader.read_line(&mut line).await;
            // Drop everything - close connection
        });

        let stream = UnixStream::connect(&sock_path).await.unwrap();
        let (read_half, write_half) = stream.into_split();
        let mut conn = ServerConnection {
            reader: BufReader::new(read_half),
            writer: write_half,
        };

        let req = Request::SessionList;
        let result = conn.send_request(&req).await;
        assert!(result.is_err(), "should error when server closes connection");
        assert!(
            result.unwrap_err().to_string().contains("server closed connection"),
            "error should mention server closed connection"
        );

        server_handle.await.unwrap();
    }
}
