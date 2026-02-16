mod handler;
mod socket;

use agentbook::client::default_socket_path;
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::NodeInbox;
use agentbook_mesh::recovery::load_or_create_recovery_key;
use agentbook_mesh::state_dir::default_state_dir;
use agentbook_mesh::transport::MeshTransport;
use anyhow::{Context, Result};
use clap::Parser;
use handler::NodeState;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(author, version, about = "agentbook node daemon")]
struct Args {
    /// Path to the Unix socket.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// State directory for node data.
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// Relay host address(es) to connect to (can be repeated).
    /// Defaults to agentbook.ardabot.ai if none specified.
    #[arg(long)]
    relay_host: Vec<String>,

    /// Disable connecting to any relay host.
    #[arg(long)]
    no_relay: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agentbook_node=info".into()),
        )
        .init();

    let args = Args::parse();

    let state_dir = args
        .state_dir
        .unwrap_or_else(|| default_state_dir().expect("failed to determine state directory"));

    let socket_path = args.socket.unwrap_or_else(default_socket_path);

    // Load or create recovery key and identity
    let recovery_key_path = state_dir.join("recovery.key");
    let kek = load_or_create_recovery_key(Some(&recovery_key_path))
        .context("failed to load recovery key")?;
    let identity =
        NodeIdentity::load_or_create(&state_dir, &kek).context("failed to load identity")?;

    tracing::info!(node_id = %identity.node_id, "node identity loaded");

    // Load follow store and inbox
    let follow_store = FollowStore::load(&state_dir).context("failed to load follow store")?;
    let inbox = NodeInbox::load(&state_dir).context("failed to load inbox")?;

    // Resolve relay hosts: use default if none specified (unless --no-relay)
    let relay_hosts = if args.no_relay {
        vec![]
    } else if args.relay_host.is_empty() {
        vec![agentbook::DEFAULT_RELAY_HOST.to_string()]
    } else {
        args.relay_host.clone()
    };

    // Set up relay transport if configured
    let transport = if !relay_hosts.is_empty() {
        let sig = identity
            .sign(identity.node_id.as_bytes())
            .context("failed to sign for relay registration")?;
        Some(MeshTransport::new(
            relay_hosts,
            identity.node_id.clone(),
            identity.public_key_b64.clone(),
            sig,
        ))
    } else {
        None
    };

    let state = NodeState::new(identity, follow_store, inbox, transport);

    // Spawn relay inbound processor
    if state.transport.is_some() {
        let state_clone = state.clone();
        tokio::spawn(async move {
            relay_inbound_loop(state_clone).await;
        });
    }

    // Run Unix socket server (blocks until shutdown signal)
    tokio::select! {
        result = socket::serve(state.clone(), &socket_path) => {
            result.context("socket server failed")?;
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received SIGINT, shutting down");
        }
    }

    // Cleanup socket file
    std::fs::remove_file(&socket_path).ok();
    tracing::info!("agentbook-node shut down");
    Ok(())
}

async fn relay_inbound_loop(state: Arc<NodeState>) {
    let transport = state.transport.as_ref().unwrap();
    let mut incoming = transport.incoming.lock().await;
    while let Some(envelope) = incoming.recv().await {
        handler::process_inbound(&state, envelope).await;
    }
}
