use agentbook_crypto::crypto::random_key_material;
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::NodeInbox;
use agentbook_mesh::transport::MeshTransport;
use agentbook_node::handler::{NodeState, WalletConfig};
use agentbook_node::socket;
use agentbook_wallet::spending_limit::SpendingLimitConfig;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

/// A test node daemon with ephemeral identity and temp directories.
pub struct TestNode {
    pub state: Arc<NodeState>,
    pub socket_path: PathBuf,
    pub node_id: String,
    pub public_key_b64: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    _state_dir: TempDir,
    _socket_dir: TempDir,
}

impl TestNode {
    /// Spawn a node connected to the given relay address.
    pub async fn spawn(relay_addr: &str) -> Result<Self> {
        let state_dir = TempDir::new()?;
        let socket_dir = TempDir::new()?;
        let socket_path = socket_dir.path().join("agentbook.sock");

        let kek = Zeroizing::new(random_key_material());

        let identity = NodeIdentity::load_or_create(state_dir.path(), &kek)
            .context("failed to create identity")?;

        let node_id = identity.node_id.clone();
        let public_key_b64 = identity.public_key_b64.clone();

        let follow_store =
            FollowStore::load(state_dir.path()).context("failed to load follow store")?;
        let inbox = NodeInbox::load(state_dir.path()).context("failed to load inbox")?;

        let relay_hosts = vec![relay_addr.to_string()];

        // Create relay transport
        let sig = identity
            .sign(identity.node_id.as_bytes())
            .context("failed to sign for relay registration")?;
        let transport = MeshTransport::new(
            relay_hosts.clone(),
            identity.node_id.clone(),
            identity.public_key_b64.clone(),
            sig,
        );

        let wallet_config = WalletConfig {
            rpc_url: "https://mainnet.base.org".to_string(),
            yolo_enabled: false,
            state_dir: state_dir.path().to_path_buf(),
            kek,
            spending_limit_config: SpendingLimitConfig::default(),
        };

        let state = NodeState::new(
            identity,
            follow_store,
            inbox,
            Some(transport),
            relay_hosts,
            wallet_config,
        );

        // Spawn relay inbound processor
        let state_for_relay = state.clone();
        tokio::spawn(async move {
            let transport = state_for_relay.transport.as_ref().unwrap();
            let mut incoming = transport.incoming.lock().await;
            while let Some(envelope) = incoming.recv().await {
                agentbook_node::handler::process_inbound(&state_for_relay, envelope).await;
            }
        });

        // Spawn socket server with shutdown
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let state_for_socket = state.clone();
        let socket_path_clone = socket_path.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = socket::serve(state_for_socket, &socket_path_clone) => {
                    if let Err(e) = result {
                        tracing::debug!(err = %e, "socket server stopped");
                    }
                }
                _ = shutdown_rx => {
                    tracing::debug!("node shutdown signal received");
                }
            }
        });

        // Wait for socket to be ready
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // Wait for relay registration to complete
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        Ok(Self {
            state,
            socket_path,
            node_id,
            public_key_b64,
            shutdown_tx: Some(shutdown_tx),
            _state_dir: state_dir,
            _socket_dir: socket_dir,
        })
    }

    /// Spawn a node without a relay connection.
    pub async fn spawn_offline() -> Result<Self> {
        let state_dir = TempDir::new()?;
        let socket_dir = TempDir::new()?;
        let socket_path = socket_dir.path().join("agentbook.sock");

        let kek = Zeroizing::new(random_key_material());

        let identity = NodeIdentity::load_or_create(state_dir.path(), &kek)
            .context("failed to create identity")?;

        let node_id = identity.node_id.clone();
        let public_key_b64 = identity.public_key_b64.clone();

        let follow_store =
            FollowStore::load(state_dir.path()).context("failed to load follow store")?;
        let inbox = NodeInbox::load(state_dir.path()).context("failed to load inbox")?;

        let wallet_config = WalletConfig {
            rpc_url: "https://mainnet.base.org".to_string(),
            yolo_enabled: false,
            state_dir: state_dir.path().to_path_buf(),
            kek,
            spending_limit_config: SpendingLimitConfig::default(),
        };

        let state = NodeState::new(identity, follow_store, inbox, None, vec![], wallet_config);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let state_for_socket = state.clone();
        let socket_path_clone = socket_path.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = socket::serve(state_for_socket, &socket_path_clone) => {
                    if let Err(e) = result {
                        tracing::debug!(err = %e, "socket server stopped");
                    }
                }
                _ = shutdown_rx => {
                    tracing::debug!("node shutdown signal received");
                }
            }
        });

        // Wait for socket to be ready
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        Ok(Self {
            state,
            socket_path,
            node_id,
            public_key_b64,
            shutdown_tx: Some(shutdown_tx),
            _state_dir: state_dir,
            _socket_dir: socket_dir,
        })
    }
}

impl Drop for TestNode {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Clean up socket file
        std::fs::remove_file(&self.socket_path).ok();
    }
}
