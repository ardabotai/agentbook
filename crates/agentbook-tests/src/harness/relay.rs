use agentbook_host::service::spawn_relay;
use anyhow::Result;
use std::net::SocketAddr;
use tempfile::TempDir;
use tokio::sync::oneshot;

/// A test relay host running on a random port with a temp data directory.
pub struct TestRelay {
    pub addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    _data_dir: TempDir,
}

impl TestRelay {
    /// Spawn a relay on a random port with an in-memory SQLite directory.
    pub async fn spawn() -> Result<Self> {
        let data_dir = TempDir::new()?;
        let (addr, shutdown_tx) = spawn_relay(Some(data_dir.path())).await?;
        Ok(Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            _data_dir: data_dir,
        })
    }

    /// Get the relay address as a string suitable for node connections.
    pub fn relay_addr(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
