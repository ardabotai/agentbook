use agentbook_crypto::recovery::key_to_mnemonic;
use agentbook_mesh::identity::NodeIdentity;
use agentbook_mesh::recovery;
use agentbook_mesh::state_dir::{default_state_dir, ensure_state_dir};
use agentbook_proto::host::v1 as host_pb;
use agentbook_proto::host::v1::host_service_client::HostServiceClient;
use anyhow::{Context, Result};
use std::path::PathBuf;

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
    let mnemonic = if has_op {
        // 1Password will store the mnemonic — skip the manual "Press Enter" prompt
        let m = display_mnemonic(&kek, "Your recovery phrase");
        eprintln!("  \x1b[1;32mThis will be saved to 1Password automatically.\x1b[0m");
        eprintln!("  Key file: {}", recovery_key_path.display());
        eprintln!();
        m
    } else {
        let m = display_and_prompt_mnemonic(&kek, "Your recovery phrase");
        eprintln!("  \x1b[1;31mNever share these words with anyone — including AI agents.\x1b[0m");
        eprintln!("  Key file: {}", recovery_key_path.display());
        eprintln!();
        m
    };

    // Step 3: Node identity
    let identity =
        NodeIdentity::load_or_create(&state_dir, &kek).context("failed to create identity")?;
    eprintln!("  \x1b[1;36mNode ID:\x1b[0m {}", identity.node_id);
    eprintln!();

    // Step 4: TOTP setup
    if has_op {
        run_totp_setup_op(&state_dir, &kek, &identity.node_id, &passphrase, mnemonic.as_deref())?;
    } else {
        run_totp_setup(&state_dir, &kek, &identity.node_id)?;
    };

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

/// Display a mnemonic phrase (no prompt to continue).
fn display_mnemonic(key: &[u8; 32], label: &str) -> Option<String> {
    let mnemonic = match key_to_mnemonic(key) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("  \x1b[1;31mFailed to generate mnemonic: {e}\x1b[0m");
            return None;
        }
    };

    print_mnemonic_words(&mnemonic, label);
    Some(mnemonic)
}

/// Display a mnemonic phrase and wait for user acknowledgment.
fn display_and_prompt_mnemonic(key: &[u8; 32], label: &str) -> Option<String> {
    let mnemonic = match key_to_mnemonic(key) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("  \x1b[1;31mFailed to generate mnemonic: {e}\x1b[0m");
            return None;
        }
    };

    print_mnemonic_words(&mnemonic, label);

    eprintln!("  Save this in an encrypted password manager (1Password, Bitwarden)");
    eprintln!("  or write it on paper and keep it somewhere safe.");
    eprintln!();
    eprintln!("  Press Enter after you've saved your recovery phrase...");
    let _ = std::io::stdin().read_line(&mut String::new());

    Some(mnemonic)
}

/// Print formatted mnemonic words to stderr.
fn print_mnemonic_words(mnemonic: &str, label: &str) {
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
}

/// Fully automated TOTP setup when 1Password is available.
///
/// Generates the TOTP secret locally, saves all secrets to 1Password in one item,
/// then auto-verifies by reading the OTP back from 1Password. No manual prompts.
fn run_totp_setup_op(
    state_dir: &std::path::Path,
    kek: &[u8; 32],
    node_id: &str,
    passphrase: &str,
    mnemonic: Option<&str>,
) -> Result<()> {
    use agentbook_wallet::onepassword;

    eprintln!("  \x1b[1;36m=== TOTP Setup (1Password) ===\x1b[0m");
    eprintln!("  Setting up two-factor authentication via 1Password.");
    eprintln!();

    let setup = agentbook_wallet::totp::generate_totp_secret(state_dir, kek, node_id)
        .context("failed to generate TOTP secret")?;

    // Save passphrase + mnemonic + TOTP to 1Password in one item
    eprintln!("  \x1b[1;36mSaving secrets to 1Password...\x1b[0m");
    let mnemonic_str = mnemonic.unwrap_or("");
    onepassword::save_agentbook_item(passphrase, mnemonic_str, &setup.otpauth_url)
        .context("failed to save secrets to 1Password")?;
    eprintln!("  \x1b[1;32mAll secrets saved to 1Password item \"agentbook\".\x1b[0m");
    eprintln!();

    // Auto-verify by reading OTP back from 1Password
    eprintln!("  Verifying TOTP via 1Password...");
    let code = onepassword::read_otp().context("failed to read OTP from 1Password")?;

    match agentbook_wallet::totp::verify_totp(state_dir, &code, kek) {
        Ok(true) => {
            eprintln!("  \x1b[1;32mTOTP verified successfully!\x1b[0m");
            eprintln!("  Future startups will auto-unlock via biometric.");
            eprintln!();
            Ok(())
        }
        Ok(false) => {
            anyhow::bail!("TOTP auto-verification failed — 1Password returned an invalid code");
        }
        Err(e) => {
            anyhow::bail!("TOTP verification error: {e}");
        }
    }
}

/// Interactive TOTP setup: show QR code, verify a code from the authenticator.
fn run_totp_setup(state_dir: &std::path::Path, kek: &[u8; 32], node_id: &str) -> Result<()> {
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

/// Validate a username string. Returns `Ok(())` if valid, `Err` with a message if not.
fn validate_username(username: &str) -> std::result::Result<(), &'static str> {
    if username.is_empty() {
        return Err("Username cannot be empty.");
    }
    if username.len() < 3 {
        return Err("Username must be at least 3 characters.");
    }
    if username.len() > 24 {
        return Err("Username must be 24 characters or less.");
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("Username can only contain letters, numbers, and underscores.");
    }
    Ok(())
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

        match validate_username(trimmed) {
            Ok(()) => return Ok(trimmed.to_string()),
            Err(msg) => {
                eprintln!("  \x1b[1;31m{msg}\x1b[0m");
                continue;
            }
        }
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

        display_and_prompt_mnemonic(&yolo_key, "Agent wallet recovery phrase");

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

#[cfg(test)]
mod tests {
    use super::*;

    // ── generate_passphrase tests ──

    #[test]
    fn passphrase_has_six_words() {
        let passphrase = generate_passphrase();
        let word_count = passphrase.split('-').count();
        assert_eq!(word_count, 6);
    }

    #[test]
    fn passphrase_meets_minimum_length() {
        // 6 words of at least 3 chars + 5 dashes = at least 23 chars, well over 8
        let passphrase = generate_passphrase();
        assert!(passphrase.len() >= 8);
    }

    #[test]
    fn passphrase_is_random() {
        let p1 = generate_passphrase();
        let p2 = generate_passphrase();
        assert_ne!(p1, p2);
    }

    #[test]
    fn passphrase_words_are_valid_bip39() {
        let wordlist = bip39::Language::English.word_list();
        let passphrase = generate_passphrase();
        for word in passphrase.split('-') {
            assert!(
                wordlist.contains(&word),
                "word '{word}' is not in BIP-39 wordlist"
            );
        }
    }

    #[test]
    fn passphrase_can_unlock_recovery_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.key");
        let passphrase = generate_passphrase();

        let created = recovery::create_recovery_key(&path, &passphrase).unwrap();
        let loaded = recovery::load_recovery_key(&path, &passphrase).unwrap();
        assert_eq!(created, loaded);
    }

    // ── validate_username tests ──

    #[test]
    fn valid_usernames() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("bob_123").is_ok());
        assert!(validate_username("ABC").is_ok());
        assert!(validate_username("a_b_c_d_e_f_g_h_i_j_k_l").is_ok()); // 23 chars
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_username("").is_err());
    }

    #[test]
    fn rejects_too_short() {
        assert!(validate_username("ab").is_err());
        assert!(validate_username("a").is_err());
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(25);
        assert!(validate_username(&long).is_err());
    }

    #[test]
    fn rejects_special_characters() {
        assert!(validate_username("alice!").is_err());
        assert!(validate_username("bob@home").is_err());
        assert!(validate_username("hello world").is_err());
        assert!(validate_username("dash-name").is_err());
        assert!(validate_username("dot.name").is_err());
    }

    #[test]
    fn accepts_boundary_lengths() {
        assert!(validate_username("abc").is_ok()); // exactly 3
        assert!(validate_username(&"a".repeat(24)).is_ok()); // exactly 24
    }
}
