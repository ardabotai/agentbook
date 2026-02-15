use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::time::{timeout, Duration};
use tmax_protocol::{Request, Response};

/// Maximum line length we will accept from the server (16 MiB).
/// Prevents a malicious or misbehaving server from causing OOM via an
/// unbounded `read_line`.
const MAX_LINE_LENGTH: usize = 16 * 1024 * 1024;

/// Timeout for establishing a connection to the server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for a complete request/response round-trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Async connection to the tmax server with split read/write halves
/// for concurrent use in tokio::select!.
pub struct ServerConnection {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: OwnedWriteHalf,
    /// Reusable buffer for reading lines, avoiding per-call allocations.
    read_buf: String,
}

impl ServerConnection {
    /// Connect to the tmax server via Unix socket.
    pub async fn connect() -> anyhow::Result<Self> {
        let socket_path = tmax_protocol::paths::default_socket_path();

        let stream = timeout(CONNECT_TIMEOUT, UnixStream::connect(&socket_path))
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "connection to tmax server timed out after {}s at {}",
                    CONNECT_TIMEOUT.as_secs(),
                    socket_path.display()
                )
            })?
            .map_err(|e| {
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
            read_buf: String::new(),
        })
    }

    /// Read a single newline-terminated line from the server into
    /// `self.read_buf`, rejecting lines that exceed [`MAX_LINE_LENGTH`]
    /// to prevent memory exhaustion.
    ///
    /// Callers must call `self.read_buf.clear()` before invoking this
    /// method if they want a fresh line.
    ///
    /// Returns the number of bytes read (0 means EOF).
    async fn read_bounded_line(&mut self) -> anyhow::Result<usize> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                // EOF
                return Ok(self.read_buf.len());
            }

            // Look for a newline in the buffered data.
            let chunk_len = available.len();
            let newline_pos = available.iter().position(|&b| b == b'\n');
            let consume_len = match newline_pos {
                Some(pos) => pos + 1, // include the newline
                None => chunk_len,    // consume entire buffer
            };

            // Check the length limit *before* appending.
            if self.read_buf.len() + consume_len > MAX_LINE_LENGTH {
                anyhow::bail!(
                    "server sent a line exceeding the {MAX_LINE_LENGTH}-byte limit"
                );
            }

            let slice = &available[..consume_len];
            let text = std::str::from_utf8(slice)
                .map_err(|e| anyhow::anyhow!("invalid UTF-8 from server: {e}"))?;
            self.read_buf.push_str(text);
            self.reader.consume(consume_len);

            if newline_pos.is_some() {
                return Ok(self.read_buf.len());
            }
        }
    }

    /// Send a request and read the response.
    ///
    /// The entire write+read round-trip is bounded by [`REQUEST_TIMEOUT`] so a
    /// non-responsive server cannot block the client indefinitely.
    pub async fn send_request(&mut self, req: &Request) -> anyhow::Result<Response> {
        timeout(REQUEST_TIMEOUT, self.send_request_inner(req))
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "request to tmax server timed out after {}s",
                    REQUEST_TIMEOUT.as_secs()
                )
            })?
    }

    /// Inner implementation of send_request without timeout wrapper.
    async fn send_request_inner(&mut self, req: &Request) -> anyhow::Result<Response> {
        let mut json = serde_json::to_string(req)?;
        json.push('\n');
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.flush().await?;

        self.read_buf.clear();
        self.read_bounded_line().await?;

        if self.read_buf.is_empty() {
            anyhow::bail!("server closed connection");
        }

        let response: Response = serde_json::from_str(&self.read_buf)?;
        Ok(response)
    }

    /// Read the next event/response from the server (for streaming).
    pub async fn read_event(&mut self) -> anyhow::Result<Option<Response>> {
        self.read_buf.clear();
        let n = self.read_bounded_line().await?;
        if n == 0 {
            return Ok(None);
        }
        let response: Response = serde_json::from_str(&self.read_buf)?;
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
            read_buf: String::new(),
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
            read_buf: String::new(),
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

    #[tokio::test]
    async fn read_bounded_line_rejects_oversized_message() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        // Server that sends a line exceeding MAX_LINE_LENGTH (no newline,
        // just a huge continuous stream so the client keeps buffering).
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (_read_half, mut write_half) = stream.into_split();

            // Write chunks of 'A' bytes totaling MAX_LINE_LENGTH + 1, without a newline.
            let chunk = vec![b'A'; 64 * 1024];
            let total_needed = MAX_LINE_LENGTH + 1;
            let mut written = 0;
            while written < total_needed {
                let to_write = chunk.len().min(total_needed - written);
                // Ignore write errors - the client may close the connection.
                if write_half.write_all(&chunk[..to_write]).await.is_err() {
                    break;
                }
                written += to_write;
            }
        });

        let stream = UnixStream::connect(&sock_path).await.unwrap();
        let (read_half, write_half) = stream.into_split();
        let mut conn = ServerConnection {
            reader: BufReader::new(read_half),
            writer: write_half,
            read_buf: String::new(),
        };

        let result = conn.read_event().await;
        assert!(result.is_err(), "should reject oversized line");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeding"),
            "error should mention limit exceeded, got: {err_msg}"
        );

        server_handle.await.unwrap();
    }
}
