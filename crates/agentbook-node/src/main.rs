use agentbook::client::default_socket_path;
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::NodeInbox;
use agentbook_mesh::recovery;
use agentbook_mesh::state_dir::default_state_dir;
use agentbook_mesh::transport::MeshTransport;
use agentbook_node::handler::{self, NodeState, WalletConfig};
use agentbook_node::socket;
use agentbook_wallet::wallet::DEFAULT_RPC_URL;
use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use zeroize::Zeroizing;

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

    /// Print READY to stdout after auth completes (used by CLI for backgrounding).
    #[arg(long, hide = true)]
    notify_ready: bool,

    /// Max ETH per yolo transaction (default: 0.01).
    #[arg(long, default_value = "0.01")]
    max_yolo_tx_eth: String,

    /// Max USDC per yolo transaction (default: 10).
    #[arg(long, default_value = "10")]
    max_yolo_tx_usdc: String,

    /// Max ETH the yolo wallet can spend per rolling 24h window (default: 0.1).
    #[arg(long, default_value = "0.1")]
    max_yolo_daily_eth: String,

    /// Max USDC the yolo wallet can spend per rolling 24h window (default: 100).
    #[arg(long, default_value = "100")]
    max_yolo_daily_usdc: String,
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

    // Require recovery key to exist — setup must be run first
    let recovery_key_path = state_dir.join("recovery.key");
    if !recovery::has_recovery_key(&recovery_key_path) {
        eprintln!();
        eprintln!("  \x1b[1;31mError: Node not set up. Run: agentbook setup\x1b[0m");
        eprintln!();
        std::process::exit(1);
    }

    let kek = load_encrypted_recovery_key(&recovery_key_path)?;

    let identity =
        NodeIdentity::load_or_create(&state_dir, &kek).context("failed to load identity")?;

    tracing::info!(node_id = %identity.node_id, "node identity loaded");

    // Require TOTP to be set up
    if !agentbook_wallet::totp::has_totp(&state_dir) {
        eprintln!();
        eprintln!("  \x1b[1;31mError: TOTP not configured. Run: agentbook setup\x1b[0m");
        eprintln!();
        std::process::exit(1);
    }

    // Verify TOTP on every startup (unless --yolo skips auth)
    if !args.yolo {
        verify_startup_totp(&state_dir, &kek)?;
    }

    // Yolo wallet: load existing key only (setup creates it)
    if args.yolo {
        if !agentbook_wallet::yolo::has_yolo_key(&state_dir) {
            eprintln!();
            eprintln!(
                "  \x1b[1;31mError: Yolo wallet not set up. Run: agentbook setup --yolo\x1b[0m"
            );
            eprintln!();
            std::process::exit(1);
        }

        let yolo_addr = agentbook_wallet::yolo::yolo_address(&state_dir)
            .context("failed to load yolo wallet")?;
        eprintln!();
        eprintln!("  \x1b[1;33m!! YOLO MODE: Agent wallet {yolo_addr} is unlocked.\x1b[0m");
        eprintln!("  \x1b[1;33m!! The agent can send transactions without approval.\x1b[0m");
        eprintln!(
            "  \x1b[1;33m!! Only fund this wallet with amounts you're comfortable losing.\x1b[0m"
        );
        eprintln!();
    }

    // Signal to the CLI that auth is complete and the node is ready to run
    if args.notify_ready {
        println!("READY");
        // Flush and close stdout so the CLI can detach
        drop(std::io::Write::flush(&mut std::io::stdout()));
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
            relay_hosts.clone(),
            identity.node_id.clone(),
            identity.public_key_b64.clone(),
            sig,
        ))
    } else {
        None
    };

    let spending_limit_config = {
        use agentbook_wallet::spending_limit::{AssetLimits, SpendingLimitConfig};
        use agentbook_wallet::wallet::{parse_eth_amount, parse_usdc_amount};

        SpendingLimitConfig {
            eth: AssetLimits {
                max_per_tx: parse_eth_amount(&args.max_yolo_tx_eth)
                    .context("invalid --max-yolo-tx-eth")?,
                max_daily: parse_eth_amount(&args.max_yolo_daily_eth)
                    .context("invalid --max-yolo-daily-eth")?,
            },
            usdc: AssetLimits {
                max_per_tx: parse_usdc_amount(&args.max_yolo_tx_usdc)
                    .context("invalid --max-yolo-tx-usdc")?,
                max_daily: parse_usdc_amount(&args.max_yolo_daily_usdc)
                    .context("invalid --max-yolo-daily-usdc")?,
            },
        }
    };

    let wallet_config = WalletConfig {
        rpc_url: args.rpc_url,
        yolo_enabled: args.yolo,
        state_dir,
        kek,
        spending_limit_config,
    };

    // Load persisted rooms
    let persisted_rooms = handler::rooms::load_rooms(&wallet_config.state_dir);

    let state = NodeState::new(
        identity,
        follow_store,
        inbox,
        transport,
        relay_hosts,
        wallet_config,
    );

    // Populate rooms from persisted config
    if !persisted_rooms.is_empty() {
        let mut rooms = state.rooms.lock().await;
        *rooms = persisted_rooms;
        tracing::info!(count = rooms.len(), "loaded persisted rooms");
    }

    // Auto sync-pull on startup if local follow store is empty (account recovery)
    if state.follow_store.lock().await.following().is_empty() && !state.relay_hosts.is_empty() {
        tracing::info!("follow store is empty — attempting auto-recovery from relay");
        match handler::social::sync_pull_from_relay(&state).await {
            Ok(result) => {
                let added = result.added.unwrap_or(0);
                if added > 0 {
                    tracing::info!(added, "recovered follows from relay");
                } else {
                    tracing::info!("no follows found on relay to recover");
                }
            }
            Err(e) => {
                tracing::warn!(err = %e, "auto sync-pull failed (non-fatal)");
            }
        }
    }

    // Auto-join #shire (the default open room) if not already joined
    if state.transport.is_some() {
        let already_joined = state.rooms.lock().await.contains_key("shire");
        if !already_joined {
            match handler::rooms::handle_join_room(&state, "shire", None).await {
                agentbook::protocol::Response::Ok { .. } => {
                    tracing::info!("auto-joined #shire");
                }
                agentbook::protocol::Response::Error { message, .. } => {
                    tracing::warn!(err = %message, "failed to auto-join #shire");
                }
                _ => {}
            }
        }
    }

    // Spawn relay inbound processor
    if state.transport.is_some() {
        // Re-subscribe to persisted rooms
        {
            let rooms = state.rooms.lock().await;
            if let Some(transport) = &state.transport {
                for room_name in rooms.keys() {
                    let frame = agentbook_proto::host::v1::NodeFrame {
                        frame: Some(
                            agentbook_proto::host::v1::node_frame::Frame::RoomSubscribe(
                                agentbook_proto::host::v1::RoomSubscribeFrame {
                                    room_id: room_name.clone(),
                                },
                            ),
                        ),
                    };
                    if let Err(e) = transport.send_control_frame(frame).await {
                        tracing::warn!(room = %room_name, err = %e, "failed to re-subscribe room on startup");
                    }
                }
            }
        }

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

/// Load and decrypt recovery key. Tries 1Password auto-fill, then falls back to manual prompt.
fn load_encrypted_recovery_key(path: &std::path::Path) -> Result<Zeroizing<[u8; 32]>> {
    use agentbook::agent_protocol::default_agent_socket_path;
    use agentbook_wallet::onepassword;

    // Try the in-memory credential agent first — no passphrase prompt needed if running.
    let agent_socket = default_agent_socket_path();
    if agent_socket.exists() {
        let kek = tokio::runtime::Handle::current().block_on(async {
            agentbook::client::AgentClient::connect(&agent_socket)
                .await?
                .get_kek()
                .await
        });
        if let Some(kek) = kek {
            eprintln!("  \x1b[1;32mUnlocked via agent.\x1b[0m");
            return Ok(kek);
        }
    }

    // Try 1Password auto-fill (derive item title from node.json in state dir)
    let state_dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let op_title = onepassword::item_title_from_state_dir(state_dir);

    if let Some(ref title) = op_title
        && onepassword::has_op_cli()
        && onepassword::has_agentbook_item(title)
    {
        eprintln!("  \x1b[1;36m1Password detected — unlocking via biometric...\x1b[0m");
        match onepassword::read_passphrase(title) {
            Ok(passphrase) => match recovery::load_recovery_key(path, &passphrase) {
                Ok(kek) => {
                    eprintln!("  \x1b[1;32mUnlocked via 1Password.\x1b[0m");
                    return Ok(kek);
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("wrong passphrase") {
                        eprintln!(
                            "  \x1b[1;31m1Password passphrase didn't match. Falling back to manual entry.\x1b[0m"
                        );
                    } else {
                        return Err(e).context("failed to load recovery key");
                    }
                }
            },
            Err(_) => {
                eprintln!(
                    "  \x1b[1;33m1Password read failed. Falling back to manual entry.\x1b[0m"
                );
            }
        }
    }

    // Manual passphrase prompt (fallback)
    loop {
        let passphrase = rpassword::prompt_password("  Enter passphrase to unlock node: ")
            .context("failed to read passphrase")?;

        match recovery::load_recovery_key(path, &passphrase) {
            Ok(kek) => {
                eprintln!("  \x1b[1;32mUnlocked.\x1b[0m");
                return Ok(kek);
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("wrong passphrase") {
                    eprintln!("  \x1b[1;31mWrong passphrase. Try again.\x1b[0m");
                } else {
                    return Err(e).context("failed to load recovery key");
                }
            }
        }
    }
}

/// Require a valid TOTP code before the node starts serving.
/// Tries 1Password auto-read first, then falls back to manual prompt.
fn verify_startup_totp(state_dir: &std::path::Path, kek: &[u8; 32]) -> Result<()> {
    use agentbook_wallet::onepassword;

    // Try 1Password TOTP auto-read
    let op_title = onepassword::item_title_from_state_dir(state_dir);
    if let Some(ref title) = op_title
        && onepassword::has_op_cli()
        && onepassword::has_agentbook_item(title)
    {
        eprintln!("  \x1b[1;36mReading TOTP from 1Password...\x1b[0m");
        match onepassword::read_otp(title) {
            Ok(code) => match agentbook_wallet::totp::verify_totp(state_dir, &code, kek) {
                Ok(true) => {
                    eprintln!("  \x1b[1;32mAuthenticated via 1Password.\x1b[0m");
                    eprintln!();
                    return Ok(());
                }
                Ok(false) => {
                    eprintln!(
                        "  \x1b[1;31m1Password TOTP code was invalid. Falling back to manual entry.\x1b[0m"
                    );
                }
                Err(e) => {
                    eprintln!(
                        "  \x1b[1;31mTOTP verification error: {e}. Falling back to manual entry.\x1b[0m"
                    );
                }
            },
            Err(_) => {
                eprintln!(
                    "  \x1b[1;33m1Password OTP read failed. Falling back to manual entry.\x1b[0m"
                );
            }
        }
    }

    // Manual TOTP prompt (fallback)
    loop {
        let code = rpassword::prompt_password("  Enter authenticator code to start node: ")
            .context("failed to read OTP code")?;
        let code = code.trim();

        match agentbook_wallet::totp::verify_totp(state_dir, code, kek) {
            Ok(true) => {
                eprintln!("  \x1b[1;32mAuthenticated.\x1b[0m");
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
