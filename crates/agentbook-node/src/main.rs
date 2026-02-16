mod handler;
mod socket;

use agentbook::client::default_socket_path;
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::NodeInbox;
use agentbook_mesh::recovery::load_or_create_recovery_key;
use agentbook_mesh::state_dir::default_state_dir;
use agentbook_mesh::transport::MeshTransport;
use agentbook_wallet::wallet::DEFAULT_RPC_URL;
use anyhow::{Context, Result};
use clap::Parser;
use handler::{NodeState, WalletConfig};
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

    /// Base chain RPC URL (default: https://mainnet.base.org).
    #[arg(long, default_value = DEFAULT_RPC_URL)]
    rpc_url: String,

    /// Enable yolo wallet for autonomous agent transactions (no auth required).
    #[arg(long)]
    yolo: bool,
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
    let first_run = !recovery_key_path.exists();
    let kek = load_or_create_recovery_key(Some(&recovery_key_path))
        .context("failed to load recovery key")?;

    if first_run {
        print_first_run_onboarding(&recovery_key_path);
    }

    let identity =
        NodeIdentity::load_or_create(&state_dir, &kek).context("failed to load identity")?;

    tracing::info!(node_id = %identity.node_id, "node identity loaded");

    // TOTP first-run: if no totp.key exists, run interactive setup
    if !agentbook_wallet::totp::has_totp(&state_dir) {
        run_totp_setup(&state_dir, &kek, &identity.node_id)?;
    }

    // Yolo wallet setup
    if args.yolo {
        let yolo_addr = agentbook_wallet::yolo::yolo_address(&state_dir)
            .context("failed to set up yolo wallet")?;
        eprintln!();
        eprintln!("  \x1b[1;33m!! YOLO MODE: Agent wallet {yolo_addr} is unlocked.\x1b[0m");
        eprintln!("  \x1b[1;33m!! The agent can send transactions without approval.\x1b[0m");
        eprintln!(
            "  \x1b[1;33m!! Only fund this wallet with amounts you're comfortable losing.\x1b[0m"
        );
        eprintln!();
    }

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

    let wallet_config = WalletConfig {
        rpc_url: args.rpc_url,
        yolo_enabled: args.yolo,
        state_dir,
        kek,
    };

    let state = NodeState::new(identity, follow_store, inbox, transport, wallet_config);

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

/// First-run onboarding: explain the recovery key and how to back it up.
fn print_first_run_onboarding(recovery_key_path: &std::path::Path) {
    eprintln!();
    eprintln!("  \x1b[1;36m=== Welcome to agentbook ===\x1b[0m");
    eprintln!();
    eprintln!("  Your recovery key has been generated at:");
    eprintln!("    \x1b[1m{}\x1b[0m", recovery_key_path.display());
    eprintln!();
    eprintln!("  \x1b[1;33mIMPORTANT: Back up this file now.\x1b[0m");
    eprintln!("  This key encrypts your identity and wallet. If you lose it, your");
    eprintln!("  node identity and funds are unrecoverable.");
    eprintln!();
    eprintln!("  Store it in a password manager (1Password, Bitwarden, etc.)");
    eprintln!("  or write it down and keep it somewhere safe.");
    eprintln!();
    eprintln!("  \x1b[1;31mNever share this key with anyone — including AI agents.\x1b[0m");
    eprintln!("  Only you should start the node. Agents cannot and should not");
    eprintln!("  access your recovery key.");
    eprintln!();
}

/// Interactive TOTP setup flow (first run only).
fn run_totp_setup(state_dir: &std::path::Path, kek: &[u8; 32], node_id: &str) -> Result<()> {
    eprintln!();
    eprintln!("  \x1b[1;36m=== TOTP Authenticator Setup ===\x1b[0m");
    eprintln!("  Setting up two-factor authentication for wallet transactions.");
    eprintln!();

    let setup = agentbook_wallet::totp::generate_totp_secret(state_dir, kek, node_id)
        .context("failed to generate TOTP secret")?;

    // Render QR code in terminal
    eprintln!("  Scan this QR code with your authenticator app:");
    eprintln!();
    if let Err(e) = qr2term::print_qr(&setup.otpauth_url) {
        eprintln!("  (QR code rendering failed: {e})");
    }
    eprintln!();
    eprintln!(
        "  Or enter this secret manually: \x1b[1m{}\x1b[0m",
        setup.secret_base32
    );
    eprintln!("  Issuer: {}", setup.issuer);
    eprintln!("  Account: {}", setup.account);
    eprintln!();

    // Verify code from authenticator
    loop {
        let code = rpassword::prompt_password("  Enter the 6-digit code from your authenticator: ")
            .context("failed to read OTP code")?;
        let code = code.trim();

        match agentbook_wallet::totp::verify_totp(state_dir, code, kek) {
            Ok(true) => {
                eprintln!("  \x1b[1;32mTOTP verified successfully!\x1b[0m");
                eprintln!();
                return Ok(());
            }
            Ok(false) => {
                eprintln!("  \x1b[1;31mInvalid code. Try again.\x1b[0m");
            }
            Err(e) => {
                eprintln!("  \x1b[1;31mVerification error: {e}\x1b[0m");
            }
        }
    }
}

async fn relay_inbound_loop(state: Arc<NodeState>) {
    let transport = state.transport.as_ref().unwrap();
    let mut incoming = transport.incoming.lock().await;
    while let Some(envelope) = incoming.recv().await {
        handler::process_inbound(&state, envelope).await;
    }
}
