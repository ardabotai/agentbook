mod handler;
mod socket;

use agentbook::client::default_socket_path;
use agentbook_mesh::follow::FollowStore;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::inbox::NodeInbox;
use agentbook_mesh::recovery;
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

    // Load or create recovery key (passphrase-encrypted at rest)
    let recovery_key_path = state_dir.join("recovery.key");
    let kek = load_or_create_encrypted_recovery_key(&recovery_key_path)?;

    let identity =
        NodeIdentity::load_or_create(&state_dir, &kek).context("failed to load identity")?;

    tracing::info!(node_id = %identity.node_id, "node identity loaded");

    // TOTP first-run: if no totp.key exists, run interactive setup
    if !agentbook_wallet::totp::has_totp(&state_dir) {
        run_totp_setup(&state_dir, &kek, &identity.node_id)?;
    }

    // Verify TOTP on every startup (unless --yolo skips auth)
    if !args.yolo {
        verify_startup_totp(&state_dir, &kek)?;
    }

    // Yolo wallet setup
    if args.yolo {
        let first_yolo = !agentbook_wallet::yolo::has_yolo_key(&state_dir);
        let yolo_addr = agentbook_wallet::yolo::yolo_address(&state_dir)
            .context("failed to set up yolo wallet")?;

        if first_yolo {
            // Show the yolo wallet mnemonic so the user can back it up
            let yolo_key = agentbook_wallet::yolo::load_yolo_key(&state_dir)
                .context("failed to load yolo key")?;
            print_yolo_onboarding(&yolo_key, &yolo_addr);
        } else {
            eprintln!();
            eprintln!("  \x1b[1;33m!! YOLO MODE: Agent wallet {yolo_addr} is unlocked.\x1b[0m");
            eprintln!("  \x1b[1;33m!! The agent can send transactions without approval.\x1b[0m");
            eprintln!(
                "  \x1b[1;33m!! Only fund this wallet with amounts you're comfortable losing.\x1b[0m"
            );
            eprintln!();
        }
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

/// Interactive recovery key setup: handles first-run and normal unlock.
fn load_or_create_encrypted_recovery_key(path: &std::path::Path) -> Result<[u8; 32]> {
    if !recovery::has_recovery_key(path) {
        // First run: create new recovery key with passphrase
        eprintln!();
        eprintln!("  \x1b[1;36m=== Welcome to agentbook ===\x1b[0m");
        eprintln!();
        eprintln!("  Choose a passphrase to protect your recovery key.");
        eprintln!("  You'll need this passphrase every time you start the node.");
        eprintln!();

        let passphrase = prompt_new_passphrase()?;
        let kek = recovery::create_recovery_key(path, &passphrase)
            .context("failed to create recovery key")?;

        display_and_backup_mnemonic(&kek, "agentbook recovery key", "Your recovery phrase");

        eprintln!("  \x1b[1;31mNever share these words with anyone — including AI agents.\x1b[0m");
        eprintln!("  Key file: {}", path.display());
        eprintln!();

        return Ok(kek);
    }

    // Normal startup: prompt for passphrase to decrypt
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

/// Prompt for a new passphrase with confirmation.
fn prompt_new_passphrase() -> Result<String> {
    loop {
        let pass1 = rpassword::prompt_password("  Enter passphrase: ")
            .context("failed to read passphrase")?;

        if pass1.len() < 8 {
            eprintln!("  \x1b[1;31mPassphrase must be at least 8 characters.\x1b[0m");
            continue;
        }

        let pass2 = rpassword::prompt_password("  Confirm passphrase: ")
            .context("failed to read passphrase")?;

        if pass1 != pass2 {
            eprintln!("  \x1b[1;31mPassphrases do not match. Try again.\x1b[0m");
            continue;
        }

        return Ok(pass1);
    }
}

/// Display a mnemonic phrase and offer to save it to 1Password.
/// Returns the mnemonic string if successful.
fn display_and_backup_mnemonic(key: &[u8; 32], title: &str, label: &str) -> Option<String> {
    let mnemonic = match agentbook_crypto::recovery::key_to_mnemonic(key) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("  \x1b[1;31mFailed to generate mnemonic: {e}\x1b[0m");
            return None;
        }
    };

    eprintln!("  {label} (24 words):");
    eprintln!();
    let words: Vec<&str> = mnemonic.split_whitespace().collect();
    for (i, chunk) in words.chunks(4).enumerate() {
        let line: Vec<String> = chunk
            .iter()
            .enumerate()
            .map(|(j, w)| format!("{:>2}. {:<12}", i * 4 + j + 1, w))
            .collect();
        eprintln!("    {}", line.join("  "));
    }
    eprintln!();

    // Offer to save to 1Password if the CLI is available
    if which_1password_cli() {
        eprintln!("  \x1b[1;36m1Password CLI detected.\x1b[0m");
        eprint!("  Save to 1Password? [Y/n] ");
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if answer.trim().is_empty() || answer.trim().eq_ignore_ascii_case("y") {
            save_to_1password(title, &mnemonic);
        }
    } else {
        eprintln!("  Save this in an encrypted password manager (1Password, Bitwarden)");
        eprintln!("  or write it on paper and keep it somewhere safe.");
        eprintln!();
        eprintln!("  Press Enter after you've saved your recovery phrase...");
        let _ = std::io::stdin().read_line(&mut String::new());
    }

    Some(mnemonic)
}

/// Check if 1Password CLI (`op`) is available.
fn which_1password_cli() -> bool {
    std::process::Command::new("op")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Save a recovery phrase to 1Password as a Secure Note.
fn save_to_1password(title: &str, mnemonic: &str) {
    let result = std::process::Command::new("op")
        .args([
            "item",
            "create",
            "--category",
            "Secure Note",
            "--title",
            title,
            &format!("notesPlain={mnemonic}"),
            "--tags",
            "agentbook,crypto,recovery",
        ])
        .status();

    match result {
        Ok(status) if status.success() => {
            eprintln!();
            eprintln!("  \x1b[1;32mSaved to 1Password as \"{title}\".\x1b[0m");
            eprintln!();
        }
        _ => {
            eprintln!();
            eprintln!("  \x1b[1;31mFailed to save to 1Password.\x1b[0m");
            eprintln!("  Please save the recovery phrase manually.");
            eprintln!();
            eprintln!("  Press Enter to continue...");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
    }
}

/// First-run yolo wallet onboarding: show the agent wallet's recovery phrase.
fn print_yolo_onboarding(yolo_key: &[u8; 32], yolo_addr: &str) {
    eprintln!();
    eprintln!("  \x1b[1;33m=== Yolo Wallet Setup ===\x1b[0m");
    eprintln!();
    eprintln!("  Agent wallet address: \x1b[1m{yolo_addr}\x1b[0m");
    eprintln!();

    display_and_backup_mnemonic(yolo_key, "agentbook yolo wallet", "Agent wallet recovery phrase");

    eprintln!("  \x1b[1;33mOnly fund this wallet with amounts you're comfortable losing.\x1b[0m");
    eprintln!("  The agent can send transactions without approval.");
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

    // Render QR code in terminal (use generate_qr_string so it goes to stderr)
    eprintln!("  Scan this QR code with your authenticator app:");
    eprintln!();
    match qr2term::generate_qr_string(&setup.otpauth_url) {
        Ok(qr) => eprint!("{qr}"),
        Err(e) => eprintln!("  (QR code rendering failed: {e})"),
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
    eprintln!("  \x1b[1mVerify your setup by entering a code from your authenticator app.\x1b[0m");
    eprintln!();
    loop {
        let code = rpassword::prompt_password("  Enter 6-digit code: ")
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

/// Require a valid TOTP code before the node starts serving.
fn verify_startup_totp(state_dir: &std::path::Path, kek: &[u8; 32]) -> Result<()> {
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
