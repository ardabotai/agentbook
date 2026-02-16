//! Integration test helpers for agentbook E2E scenarios.
//!
//! These helpers will be fleshed out once `agentbook-node` is built.
//! For now the crate compiles cleanly so the workspace is valid.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::Duration;

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

/// Spawn an agentbook-host relay binary.
pub async fn spawn_host() -> Result<(std::net::SocketAddr, tokio::process::Child)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    drop(listener);

    let bin = bin_path("agentbook-host");
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
    bail!("agentbook-host failed to start at {addr}");
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

/// Extract an array from nested JSON path.
pub fn extract_array(data: &Option<serde_json::Value>, path: &[&str]) -> Vec<serde_json::Value> {
    let mut v = data.as_ref().unwrap().clone();
    for key in path {
        v = v.get(*key).unwrap().clone();
    }
    v.as_array().unwrap().clone()
}
