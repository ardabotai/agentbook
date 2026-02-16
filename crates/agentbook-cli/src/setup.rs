use agentbook_crypto::recovery::key_to_mnemonic;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::recovery;
use agentbook_mesh::state_dir::{default_state_dir, ensure_state_dir};
use agentbook_proto::host::v1 as host_pb;
use agentbook_proto::host::v1::host_service_client::HostServiceClient;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Temporary stash for first-run secrets, used to defer 1Password item creation
/// until after TOTP setup so all three fields go into one item.
#[derive(serde::Serialize, serde::Deserialize)]
struct FirstRunStash {
    passphrase: String,
    mnemonic: String,
}

/// Run interactive first-time setup.
pub async fn cmd_setup(yolo: bool, state_dir: Option<PathBuf>) -> Result<()> {
    let state_dir =
        state_dir.unwrap_or_else(|| default_state_dir().expect("failed to determine state dir"));
    ensure_state_dir(&state_dir)?;

    let recovery_key_path = state_dir.join("recovery.key");

    // Idempotency check: bail if already set up
    if recovery::has_recovery_key(&recovery_key_path)
        && agentbook_wallet::totp::has_totp(&state_dir)
    {
        eprintln!();
        eprintln!("  \x1b[1;32mNode already set up. Use `agentbook up` to start.\x1b[0m");
        eprintln!();
        return Ok(());
    }

    eprintln!();
    eprintln!("  \x1b[1;36m=== Welcome to agentbook ===\x1b[0m");
    eprintln!();

    // Step 1: Passphrase
    let has_op = agentbook_wallet::onepassword::has_op_cli();
    let passphrase = if has_op {
        let generated = generate_passphrase();
        eprintln!("  \x1b[1;32m1Password detected — passphrase auto-generated.\x1b[0m");
        eprintln!("  It will be saved to 1Password after TOTP setup.");
        eprintln!();
        generated
    } else {
        eprintln!("  Choose a passphrase to protect your recovery key.");
        eprintln!("  You'll need this passphrase every time you start the node.");
        eprintln!();
        prompt_new_passphrase()?
    };
    let kek = recovery::create_recovery_key(&recovery_key_path, &passphrase)
        .context("failed to create recovery key")?;

    // Step 2: Mnemonic backup
    let mnemonic = display_and_backup_mnemonic(&kek, "Your recovery phrase");
    eprintln!("  \x1b[1;31mNever share these words with anyone — including AI agents.\x1b[0m");
    eprintln!("  Key file: {}", recovery_key_path.display());
    eprintln!();

    // Stash passphrase + mnemonic for 1Password save after TOTP setup
    if let Some(ref mnemonic) = mnemonic
        && has_op
    {
        let stash = FirstRunStash {
            passphrase: passphrase.clone(),
            mnemonic: mnemonic.clone(),
        };
        let stash_path = state_dir.join(".op_first_run_stash");
        let json = serde_json::to_vec(&stash).unwrap();
        std::fs::write(&stash_path, json).ok();
    }

    // Step 3: Node identity
    let identity =
        NodeIdentity::load_or_create(&state_dir, &kek).context("failed to create identity")?;
    eprintln!("  \x1b[1;36mNode ID:\x1b[0m {}", identity.node_id);
    eprintln!();

    // Step 4: TOTP setup
    let otpauth_url = run_totp_setup(&state_dir, &kek, &identity.node_id)?;

    // Step 5: 1Password save
    save_first_run_to_1password(&state_dir, &otpauth_url);

    // Step 6: Username registration
    register_username_interactive(&identity).await?;

    // Step 7: Yolo wallet (optional)
    if yolo {
        setup_yolo_wallet(&state_dir)?;
    }

    eprintln!("  \x1b[1;32mSetup complete. Run `agentbook up` to start the node.\x1b[0m");
    eprintln!();
    Ok(())
}

/// Generate a strong random passphrase (6 BIP-39 words separated by dashes).
fn generate_passphrase() -> String {
    use rand::Rng;
    let wordlist = bip39::Language::English.word_list();
    let mut rng = rand::thread_rng();
    let words: Vec<&str> = (0..6).map(|_| wordlist[rng.gen_range(0..2048)]).collect();
    words.join("-")
}

/// Prompt for a new passphrase with confirmation and 8+ char minimum.
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

/// Display a mnemonic phrase and wait for user acknowledgment.
fn display_and_backup_mnemonic(key: &[u8; 32], label: &str) -> Option<String> {
    let mnemonic = match key_to_mnemonic(key) {
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

/// Interactive TOTP setup: show QR code, verify a code from the authenticator.
fn run_totp_setup(state_dir: &std::path::Path, kek: &[u8; 32], node_id: &str) -> Result<String> {
    eprintln!("  \x1b[1;36m=== TOTP Authenticator Setup ===\x1b[0m");
    eprintln!("  Setting up two-factor authentication for wallet transactions.");
    eprintln!();

    let setup = agentbook_wallet::totp::generate_totp_secret(state_dir, kek, node_id)
        .context("failed to generate TOTP secret")?;

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
                return Ok(setup.otpauth_url);
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

/// After TOTP setup, create the unified 1Password item with all secrets.
fn save_first_run_to_1password(state_dir: &std::path::Path, otpauth_url: &str) {
    use agentbook_wallet::onepassword;

    let stash_path = state_dir.join(".op_first_run_stash");
    if !stash_path.exists() {
        return;
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
            eprintln!("  You can set this up later by re-running setup.");
            eprintln!();
        }
    }
}

/// Prompt for a username and register it on the relay host.
/// Keeps prompting until a valid, available username is chosen.
async fn register_username_interactive(identity: &NodeIdentity) -> Result<()> {
    eprintln!("  \x1b[1;36m=== Username Registration ===\x1b[0m");
    eprintln!("  Choose a username for the agentbook network.");
    eprintln!();

    let relay_endpoint = format!("http://{}", agentbook::DEFAULT_RELAY_HOST);

    let mut client = match HostServiceClient::connect(relay_endpoint.clone()).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "  \x1b[1;33mCould not connect to relay for username registration: {e}\x1b[0m"
            );
            eprintln!("  You can register a username later with: agentbook register <username>");
            eprintln!();
            return Ok(());
        }
    };

    let sig = identity
        .sign(identity.node_id.as_bytes())
        .context("failed to sign for username registration")?;

    loop {
        let username = prompt_username()?;

        let resp = client
            .register_username(host_pb::RegisterUsernameRequest {
                username: username.clone(),
                node_id: identity.node_id.clone(),
                public_key_b64: identity.public_key_b64.clone(),
                signature_b64: sig.clone(),
            })
            .await;

        match resp {
            Ok(inner) => {
                let r = inner.into_inner();
                if r.success {
                    eprintln!("  \x1b[1;32mRegistered as @{username}\x1b[0m");
                    eprintln!();
                    return Ok(());
                }
                let err = r.error.unwrap_or_default();
                eprintln!("  \x1b[1;31m{err}\x1b[0m");
                eprintln!("  Try a different username.");
                eprintln!();
            }
            Err(e) => {
                eprintln!("  \x1b[1;31mRegistration failed: {e}\x1b[0m");
                eprintln!("  You can register later with: agentbook register <username>");
                eprintln!();
                return Ok(());
            }
        }
    }
}

/// Prompt for a username (non-hidden, normal text input).
fn prompt_username() -> Result<String> {
    use std::io::Write;
    loop {
        eprint!("  Enter username (letters, numbers, underscores): @");
        std::io::stderr().flush().ok();
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("failed to read username")?;
        let trimmed = input.trim();

        if trimmed.is_empty() {
            eprintln!("  \x1b[1;31mUsername cannot be empty.\x1b[0m");
            continue;
        }

        if trimmed.len() < 3 {
            eprintln!("  \x1b[1;31mUsername must be at least 3 characters.\x1b[0m");
            continue;
        }

        if trimmed.len() > 24 {
            eprintln!("  \x1b[1;31mUsername must be 24 characters or less.\x1b[0m");
            continue;
        }

        if !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            eprintln!(
                "  \x1b[1;31mUsername can only contain letters, numbers, and underscores.\x1b[0m"
            );
            continue;
        }

        return Ok(trimmed.to_string());
    }
}

/// Set up the yolo wallet and display the recovery phrase.
fn setup_yolo_wallet(state_dir: &std::path::Path) -> Result<()> {
    let first_yolo = !agentbook_wallet::yolo::has_yolo_key(state_dir);
    let yolo_addr =
        agentbook_wallet::yolo::yolo_address(state_dir).context("failed to set up yolo wallet")?;

    if first_yolo {
        let yolo_key =
            agentbook_wallet::yolo::load_yolo_key(state_dir).context("failed to load yolo key")?;

        eprintln!();
        eprintln!("  \x1b[1;33m=== Yolo Wallet Setup ===\x1b[0m");
        eprintln!();
        eprintln!("  Agent wallet address: \x1b[1m{yolo_addr}\x1b[0m");
        eprintln!();

        display_and_backup_mnemonic(&yolo_key, "Agent wallet recovery phrase");

        eprintln!(
            "  \x1b[1;33mOnly fund this wallet with amounts you're comfortable losing.\x1b[0m"
        );
        eprintln!("  The agent can send transactions without approval.");
        eprintln!();

        // Save yolo mnemonic to 1Password if available
        if agentbook_wallet::onepassword::has_op_cli()
            && agentbook_wallet::onepassword::has_agentbook_item()
            && let Ok(mnemonic) = key_to_mnemonic(&yolo_key)
        {
            if let Err(e) = agentbook_wallet::onepassword::save_yolo_mnemonic(&mnemonic) {
                eprintln!("  \x1b[1;33mFailed to save yolo mnemonic to 1Password: {e}\x1b[0m");
            } else {
                eprintln!("  \x1b[1;32mYolo mnemonic saved to 1Password.\x1b[0m");
                eprintln!();
            }
        }
    } else {
        eprintln!("  \x1b[1;32mYolo wallet already set up: {yolo_addr}\x1b[0m");
        eprintln!();
    }

    Ok(())
}
