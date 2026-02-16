//! Integration test helpers for tmax-mesh E2E scenarios.

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tmax_protocol::{MAX_JSON_LINE_BYTES, Request, Response};
use tokio::net::UnixStream;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

/// JSON-lines client for tmax-node's Unix socket API.
pub struct NodeClient {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
    writer: FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
}

impl NodeClient {
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("failed to connect {}", socket_path.display()))?;
        let (r, w) = stream.into_split();
        let mut client = Self {
            reader: FramedRead::new(r, LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES)),
            writer: FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES)),
        };
        match client.next_response().await? {
            Response::Hello { .. } => Ok(client),
            other => Err(anyhow!("expected Hello, got {other:?}")),
        }
    }

    pub async fn send(&mut self, req: Request) -> Result<()> {
        let line = serde_json::to_string(&req)?;
        self.writer.send(line).await?;
        Ok(())
    }

    pub async fn next_response(&mut self) -> Result<Response> {
        let Some(line) = self.reader.next().await else {
            bail!("server disconnected");
        };
        Ok(serde_json::from_str(&line?)?)
    }

    pub async fn request_ok(&mut self, req: Request) -> Result<Option<serde_json::Value>> {
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

/// Extract a string value from nested JSON path.
pub fn extract_str(data: &Option<serde_json::Value>, path: &[&str]) -> String {
    let mut v = data.as_ref().unwrap().clone();
    for key in path {
        v = v.get(*key).unwrap().clone();
    }
    v.as_str().unwrap().to_string()
}

/// Extract a bool value from nested JSON path.
pub fn extract_bool(data: &Option<serde_json::Value>, path: &[&str]) -> bool {
    let mut v = data.as_ref().unwrap().clone();
    for key in path {
        v = v.get(*key).unwrap().clone();
    }
    v.as_bool().unwrap()
}

/// Extract an array from nested JSON path, or directly if path is empty.
pub fn extract_array(data: &Option<serde_json::Value>, path: &[&str]) -> Vec<serde_json::Value> {
    let mut v = data.as_ref().unwrap().clone();
    for key in path {
        v = v.get(*key).unwrap().clone();
    }
    v.as_array().unwrap().clone()
}

fn bin_path(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push(name);
    if path.exists() {
        return path;
    }
    PathBuf::from(format!("target/debug/{name}"))
}

/// Spawn a tmax-node with Unix socket + optional peer-listen and relay-host.
pub async fn spawn_node(
    peer_listen: bool,
    relay_host: Option<std::net::SocketAddr>,
) -> Result<SpawnedNode> {
    let dir = tempfile::tempdir()?;
    let socket_path = dir.path().join("tmax-node.sock");

    let bin = bin_path("tmax-node");
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("--socket")
        .arg(&socket_path)
        .arg("--state-dir")
        .arg(dir.path().to_str().unwrap())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let peer_addr = if peer_listen {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        drop(listener);
        cmd.arg("--peer-listen").arg(addr.to_string());
        Some(addr)
    } else {
        None
    };

    if let Some(relay) = relay_host {
        cmd.arg("--relay-host").arg(relay.to_string());
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {}", bin.display()))?;

    // Wait for socket to become connectable
    for _ in 0..100 {
        if socket_path.exists()
            && let Ok(s) = UnixStream::connect(&socket_path).await
        {
            drop(s);
            return Ok(SpawnedNode {
                socket_path,
                peer_addr,
                _dir: dir,
                child,
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!(
        "tmax-node failed to start within 5s at {}",
        socket_path.display()
    );
}

pub struct SpawnedNode {
    pub socket_path: PathBuf,
    pub peer_addr: Option<std::net::SocketAddr>,
    pub _dir: tempfile::TempDir,
    pub child: tokio::process::Child,
}

/// Spawn a tmax-host relay binary.
pub async fn spawn_host() -> Result<(std::net::SocketAddr, tokio::process::Child)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    drop(listener);

    let bin = bin_path("tmax-host");
    let child = tokio::process::Command::new(&bin)
        .arg("--listen")
        .arg(addr.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn {}", bin.display()))?;

    for _ in 0..100 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return Ok((addr, child));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("tmax-host failed to start at {addr}");
}
