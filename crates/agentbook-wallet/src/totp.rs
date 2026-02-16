use anyhow::{Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::path::Path;
use totp_rs::{Algorithm, Secret, TOTP};

const TOTP_KEY_FILE: &str = "totp.key";
const TOTP_ISSUER: &str = "agentbook";
const TOTP_DIGITS: usize = 6;
const TOTP_STEP: u64 = 30;
const NONCE_LEN: usize = 12;

/// Information returned to the CLI/TUI for displaying TOTP setup to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpSetup {
    /// Base32-encoded secret for manual entry into authenticator apps.
    pub secret_base32: String,
    /// otpauth:// URL for QR code generation.
    pub otpauth_url: String,
    /// Issuer name shown in authenticator app.
    pub issuer: String,
    /// Account name shown in authenticator app (node_id or @username).
    pub account: String,
}

/// Encrypted TOTP secret stored on disk.
#[derive(Serialize, Deserialize)]
struct EncryptedTotpSecret {
    ciphertext: Vec<u8>,
    nonce: [u8; NONCE_LEN],
}

/// Derive a 32-byte key encryption key from a passphrase using Argon2id.
pub fn derive_kek_from_passphrase(passphrase: &str, salt: &[u8]) -> Result<[u8; 32]> {
    use argon2::Argon2;

    let mut kek = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut kek)
        .map_err(|e| anyhow::anyhow!("argon2 key derivation failed: {e}"))?;
    Ok(kek)
}

/// Generate a new TOTP secret, encrypt it with the KEK, and save to disk.
/// Returns setup info for the user to scan with their authenticator app.
pub fn generate_totp_secret(state_dir: &Path, kek: &[u8; 32], account: &str) -> Result<TotpSetup> {
    let secret = Secret::generate_secret();
    let secret_bytes = secret
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("failed to get secret bytes: {e}"))?;
    let secret_base32 = secret.to_encoded().to_string();

    let totp = build_totp(&secret_bytes, account)?;
    let otpauth_url = totp.get_url();

    // Encrypt and save the secret
    save_encrypted_secret(state_dir, kek, &secret_bytes)?;

    Ok(TotpSetup {
        secret_base32,
        otpauth_url,
        issuer: TOTP_ISSUER.to_string(),
        account: account.to_string(),
    })
}

/// Verify a 6-digit TOTP code against the stored encrypted secret.
pub fn verify_totp(state_dir: &Path, code: &str, kek: &[u8; 32]) -> Result<bool> {
    let secret_bytes = load_encrypted_secret(state_dir, kek)?;
    let totp = build_totp_verify_only(&secret_bytes)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("failed to get current time")?
        .as_secs();

    Ok(totp.check(code, now))
}

/// Check whether TOTP has been configured (totp.key file exists).
pub fn has_totp(state_dir: &Path) -> bool {
    state_dir.join(TOTP_KEY_FILE).exists()
}

/// Build a TOTP instance for code generation and URL creation.
fn build_totp(secret_bytes: &[u8], account: &str) -> Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        TOTP_DIGITS,
        1, // skew (allow 1 step before/after)
        TOTP_STEP,
        secret_bytes.to_vec(),
        Some(TOTP_ISSUER.to_string()),
        account.to_string(),
    )
    .map_err(|e| anyhow::anyhow!("failed to build TOTP: {e}"))
}

/// Build a TOTP instance for verification only (no account/issuer needed).
fn build_totp_verify_only(secret_bytes: &[u8]) -> Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        TOTP_DIGITS,
        1,
        TOTP_STEP,
        secret_bytes.to_vec(),
        Some(TOTP_ISSUER.to_string()),
        String::new(),
    )
    .map_err(|e| anyhow::anyhow!("failed to build TOTP for verification: {e}"))
}

/// Encrypt the TOTP secret with the KEK and save to state_dir/totp.key.
fn save_encrypted_secret(state_dir: &Path, kek: &[u8; 32], secret: &[u8]) -> Result<()> {
    let cipher =
        ChaCha20Poly1305::new_from_slice(kek).context("invalid KEK length for ChaCha20")?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, secret)
        .map_err(|_| anyhow::anyhow!("failed to encrypt TOTP secret"))?;

    let encrypted = EncryptedTotpSecret {
        ciphertext,
        nonce: nonce_bytes,
    };

    let json = serde_json::to_vec(&encrypted).context("failed to serialize encrypted secret")?;
    let path = state_dir.join(TOTP_KEY_FILE);
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("failed to set totp.key permissions")?;
    }

    Ok(())
}

/// Load and decrypt the TOTP secret from state_dir/totp.key.
fn load_encrypted_secret(state_dir: &Path, kek: &[u8; 32]) -> Result<Vec<u8>> {
    let path = state_dir.join(TOTP_KEY_FILE);
    let json =
        std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;

    let encrypted: EncryptedTotpSecret =
        serde_json::from_slice(&json).context("failed to parse encrypted TOTP secret")?;

    let cipher =
        ChaCha20Poly1305::new_from_slice(kek).context("invalid KEK length for ChaCha20")?;
    let nonce = Nonce::from_slice(&encrypted.nonce);

    cipher
        .decrypt(nonce, encrypted.ciphertext.as_ref())
        .map_err(|_| anyhow::anyhow!("failed to decrypt TOTP secret — wrong passphrase?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_verify_totp() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        let setup = generate_totp_secret(dir.path(), &kek, "test-node").unwrap();

        assert!(!setup.secret_base32.is_empty());
        assert!(setup.otpauth_url.starts_with("otpauth://totp/"));
        assert_eq!(setup.issuer, "agentbook");
        assert!(has_totp(dir.path()));

        // Generate a valid code from the stored secret
        let secret_bytes = load_encrypted_secret(dir.path(), &kek).unwrap();
        let totp = build_totp_verify_only(&secret_bytes).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp.generate(now);

        assert!(verify_totp(dir.path(), &code, &kek).unwrap());
    }

    #[test]
    fn verify_wrong_code_fails() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();
        assert!(!verify_totp(dir.path(), "000000", &kek).unwrap());
    }

    #[test]
    fn verify_wrong_kek_fails() {
        let dir = tempfile::tempdir().unwrap();
        let kek = [0x42u8; 32];
        let wrong_kek = [0x99u8; 32];

        generate_totp_secret(dir.path(), &kek, "test-node").unwrap();
        assert!(verify_totp(dir.path(), "123456", &wrong_kek).is_err());
    }

    #[test]
    fn has_totp_false_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_totp(dir.path()));
    }

    #[test]
    fn derive_kek_deterministic() {
        let salt = b"test-salt-16byte";
        let kek1 = derive_kek_from_passphrase("my-passphrase", salt).unwrap();
        let kek2 = derive_kek_from_passphrase("my-passphrase", salt).unwrap();
        assert_eq!(kek1, kek2);
    }

    #[test]
    fn derive_kek_different_passphrases() {
        let salt = b"test-salt-16byte";
        let kek1 = derive_kek_from_passphrase("pass1", salt).unwrap();
        let kek2 = derive_kek_from_passphrase("pass2", salt).unwrap();
        assert_ne!(kek1, kek2);
    }
}
