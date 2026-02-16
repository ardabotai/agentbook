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
    let kek = load_or_create_encrypted_recovery_key(&recovery_key_path, &state_dir)?;

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

            // Save yolo mnemonic to 1Password if the item exists
            if agentbook_wallet::onepassword::has_op_cli()
                && agentbook_wallet::onepassword::has_agentbook_item()
                && let Ok(mnemonic) = agentbook_crypto::recovery::key_to_mnemonic(&yolo_key)
            {
                if let Err(e) = agentbook_wallet::onepassword::save_yolo_mnemonic(&mnemonic) {
                    eprintln!("  \x1b[1;33mFailed to save yolo mnemonic to 1Password: {e}\x1b[0m");
                } else {
                    eprintln!("  \x1b[1;32mYolo mnemonic saved to 1Password.\x1b[0m");
                    eprintln!();
                }
            }
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
/// On first run, creates the recovery key and optionally saves all secrets to 1Password.
/// On subsequent runs, tries 1Password auto-fill before falling back to manual prompt.
fn load_or_create_encrypted_recovery_key(
    path: &std::path::Path,
    state_dir: &std::path::Path,
) -> Result<[u8; 32]> {
    use agentbook_wallet::onepassword;

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

        let mnemonic =
            display_and_backup_mnemonic(&kek, "agentbook recovery key", "Your recovery phrase");

        eprintln!("  \x1b[1;31mNever share these words with anyone — including AI agents.\x1b[0m");
        eprintln!("  Key file: {}", path.display());
        eprintln!();

        // Defer 1Password item creation until after TOTP setup so we can
        // include the otpauth URL in the same item. Stash the passphrase and
        // mnemonic in a temporary file that `save_first_run_to_1password` reads.
        if let Some(ref mnemonic) = mnemonic
            && onepassword::has_op_cli()
        {
            let stash = FirstRunStash {
                passphrase: passphrase.clone(),
                mnemonic: mnemonic.clone(),
            };
            let stash_path = state_dir.join(".op_first_run_stash");
            let json = serde_json::to_vec(&stash).unwrap();
            std::fs::write(&stash_path, json).ok();
        }

        return Ok(kek);
    }

    // Normal startup: try 1Password auto-fill, then fall back to manual prompt
    if onepassword::has_op_cli() && onepassword::has_agentbook_item() {
        eprintln!("  \x1b[1;36m1Password detected — unlocking via biometric...\x1b[0m");
        match onepassword::read_passphrase() {
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
fn display_and_backup_mnemonic(key: &[u8; 32], _title: &str, label: &str) -> Option<String> {
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

    eprintln!("  Save this in an encrypted password manager (1Password, Bitwarden)");
    eprintln!("  or write it on paper and keep it somewhere safe.");
    eprintln!();
    eprintln!("  Press Enter after you've saved your recovery phrase...");
    let _ = std::io::stdin().read_line(&mut String::new());

    Some(mnemonic)
}

/// Temporary stash for first-run secrets, used to defer 1Password item creation
/// until after TOTP setup so all three fields go into one item.
#[derive(serde::Serialize, serde::Deserialize)]
struct FirstRunStash {
    passphrase: String,
    mnemonic: String,
}

/// First-run yolo wallet onboarding: show the agent wallet's recovery phrase.
fn print_yolo_onboarding(yolo_key: &[u8; 32], yolo_addr: &str) {
    eprintln!();
    eprintln!("  \x1b[1;33m=== Yolo Wallet Setup ===\x1b[0m");
    eprintln!();
    eprintln!("  Agent wallet address: \x1b[1m{yolo_addr}\x1b[0m");
    eprintln!();

    display_and_backup_mnemonic(
        yolo_key,
        "agentbook yolo wallet",
        "Agent wallet recovery phrase",
    );

    eprintln!("  \x1b[1;33mOnly fund this wallet with amounts you're comfortable losing.\x1b[0m");
    eprintln!("  The agent can send transactions without approval.");
    eprintln!();
}

/// Interactive TOTP setup flow (first run only).
/// After verification, saves all first-run secrets to 1Password if available.
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

                // Now that TOTP is set up, save everything to 1Password
                save_first_run_to_1password(state_dir, &setup.otpauth_url);

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

/// After first-run TOTP setup, create the unified 1Password item with all secrets.
/// Reads the stashed passphrase/mnemonic from the temporary file, then cleans up.
fn save_first_run_to_1password(state_dir: &std::path::Path, otpauth_url: &str) {
    use agentbook_wallet::onepassword;

    let stash_path = state_dir.join(".op_first_run_stash");
    if !stash_path.exists() {
        return; // No stash = 1Password wasn't available at first run
    }

    let data = match std::fs::read(&stash_path) {
        Ok(d) => d,
        Err(_) => return,
    };
    // Always clean up the stash file (contains the passphrase)
    std::fs::remove_file(&stash_path).ok();

    let stash: FirstRunStash = match serde_json::from_slice(&data) {
        Ok(s) => s,
        Err(_) => return,
    };

    eprintln!("  \x1b[1;36mSaving secrets to 1Password...\x1b[0m");
    match onepassword::save_agentbook_item(&stash.passphrase, &stash.mnemonic, otpauth_url) {
        Ok(()) => {
            eprintln!("  \x1b[1;32mAll secrets saved to 1Password item \"agentbook\".\x1b[0m");
            eprintln!("  Future startups will auto-unlock via biometric.\x1b[0m");
            eprintln!();
        }
        Err(e) => {
            eprintln!("  \x1b[1;31mFailed to save to 1Password: {e}\x1b[0m");
            eprintln!("  You can set this up later by re-running onboarding.");
            eprintln!();
        }
    }
}

/// Require a valid TOTP code before the node starts serving.
/// Tries 1Password auto-read first, then falls back to manual prompt.
fn verify_startup_totp(state_dir: &std::path::Path, kek: &[u8; 32]) -> Result<()> {
    use agentbook_wallet::onepassword;

    // Try 1Password TOTP auto-read
    if onepassword::has_op_cli() && onepassword::has_agentbook_item() {
        eprintln!("  \x1b[1;36mReading TOTP from 1Password...\x1b[0m");
        match onepassword::read_otp() {
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
