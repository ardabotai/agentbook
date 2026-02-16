use crate::crypto::{ENVELOPE_KEY_BYTES, random_key_material};
use anyhow::{Context, Result, bail};
use bip39::Mnemonic;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;

/// On-disk format for an encrypted recovery key.
#[derive(Serialize, Deserialize)]
struct EncryptedRecoveryKey {
    /// Argon2id salt (hex).
    salt: String,
    /// ChaCha20-Poly1305 nonce (hex).
    nonce: String,
    /// Encrypted KEK (hex).
    ciphertext: String,
    /// Format version for future compatibility.
    version: u32,
}

/// Derive a 32-byte key from a passphrase + salt using Argon2id.
pub fn derive_key_from_passphrase(passphrase: &str, salt: &[u8]) -> Result<[u8; 32]> {
    use argon2::Argon2;

    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("argon2 key derivation failed: {e}"))?;
    Ok(key)
}

/// Generate a new recovery key, encrypt it with the passphrase, and save to disk.
///
/// Returns the raw 32-byte KEK.
pub fn create_recovery_key(path: &Path, passphrase: &str) -> Result<[u8; 32]> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create recovery key directory {}",
                parent.display()
            )
        })?;
    }

    let kek = random_key_material();
    save_encrypted_recovery_key(path, passphrase, &kek)?;
    Ok(kek)
}

/// Load and decrypt a recovery key from disk using the passphrase.
pub fn load_recovery_key(path: &Path, passphrase: &str) -> Result<[u8; 32]> {
    let json = fs::read_to_string(path)
        .with_context(|| format!("failed to read recovery key {}", path.display()))?;

    let encrypted: EncryptedRecoveryKey =
        serde_json::from_str(&json).context("invalid recovery key format")?;

    if encrypted.version != 1 {
        bail!(
            "unsupported recovery key version: {}",
            encrypted.version
        );
    }

    let salt = hex::decode(&encrypted.salt).context("invalid salt hex")?;
    let nonce_bytes = hex::decode(&encrypted.nonce).context("invalid nonce hex")?;
    let ciphertext = hex::decode(&encrypted.ciphertext).context("invalid ciphertext hex")?;

    if nonce_bytes.len() != NONCE_LEN {
        bail!("invalid nonce length");
    }

    let wrapping_key = derive_key_from_passphrase(passphrase, &salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&wrapping_key)
        .context("invalid wrapping key length")?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| anyhow::anyhow!("failed to decrypt recovery key — wrong passphrase?"))?;

    if plaintext.len() != ENVELOPE_KEY_BYTES {
        bail!(
            "recovery key has invalid length: expected {} bytes, got {}",
            ENVELOPE_KEY_BYTES,
            plaintext.len()
        );
    }

    let mut kek = [0u8; ENVELOPE_KEY_BYTES];
    kek.copy_from_slice(&plaintext);
    Ok(kek)
}

/// Encrypt and save a recovery key to disk.
fn save_encrypted_recovery_key(path: &Path, passphrase: &str, kek: &[u8; 32]) -> Result<()> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);

    let wrapping_key = derive_key_from_passphrase(passphrase, &salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&wrapping_key)
        .context("invalid wrapping key length")?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, kek.as_ref())
        .map_err(|_| anyhow::anyhow!("failed to encrypt recovery key"))?;

    let encrypted = EncryptedRecoveryKey {
        salt: hex::encode(salt),
        nonce: hex::encode(nonce_bytes),
        ciphertext: hex::encode(ciphertext),
        version: 1,
    };

    let json = serde_json::to_string_pretty(&encrypted)?;
    fs::write(path, &json)
        .with_context(|| format!("failed to write recovery key {}", path.display()))?;

    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!(
                "failed to set recovery key permissions {}",
                path.display()
            )
        })?;
    }

    Ok(())
}

/// Check if a recovery key file exists at the given path.
pub fn has_recovery_key(path: &Path) -> bool {
    path.exists()
}

/// Generate an ephemeral (in-memory) recovery key. Not persisted to disk.
pub fn ephemeral_recovery_key() -> [u8; ENVELOPE_KEY_BYTES] {
    random_key_material()
}

/// Convert a 32-byte recovery key to a 24-word BIP-39 mnemonic phrase.
pub fn key_to_mnemonic(key: &[u8; ENVELOPE_KEY_BYTES]) -> Result<String> {
    let mnemonic = Mnemonic::from_entropy(key)
        .map_err(|e| anyhow::anyhow!("failed to create mnemonic: {e}"))?;
    Ok(mnemonic.to_string())
}

/// Convert a 24-word BIP-39 mnemonic phrase back to a 32-byte recovery key.
pub fn mnemonic_to_key(phrase: &str) -> Result<[u8; ENVELOPE_KEY_BYTES]> {
    let mnemonic: Mnemonic = phrase
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid mnemonic: {e}"))?;
    let entropy = mnemonic.to_entropy();
    if entropy.len() != ENVELOPE_KEY_BYTES {
        bail!(
            "mnemonic entropy is {} bytes, expected {ENVELOPE_KEY_BYTES}",
            entropy.len()
        );
    }
    let mut key = [0u8; ENVELOPE_KEY_BYTES];
    key.copy_from_slice(&entropy);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn ephemeral_key_is_random() {
        let k1 = ephemeral_recovery_key();
        let k2 = ephemeral_recovery_key();
        assert_ne!(k1, k2);
    }

    #[test]
    fn create_then_load_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.key");
        let passphrase = "test-passphrase-123";

        let created = create_recovery_key(&path, passphrase).unwrap();
        let loaded = load_recovery_key(&path, passphrase).unwrap();
        assert_eq!(created, loaded);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.key");

        create_recovery_key(&path, "correct-pass").unwrap();
        let result = load_recovery_key(&path, "wrong-pass");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("wrong passphrase")
        );
    }

    #[test]
    fn encrypted_file_is_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.key");
        create_recovery_key(&path, "pass").unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(parsed.get("salt").is_some());
        assert!(parsed.get("nonce").is_some());
        assert!(parsed.get("ciphertext").is_some());
        assert_eq!(parsed.get("version").unwrap().as_u64().unwrap(), 1);
    }

    #[test]
    fn encrypted_file_not_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.key");
        let kek = create_recovery_key(&path, "pass").unwrap();

        // The raw file should NOT contain the base64-encoded key
        let raw = fs::read_to_string(&path).unwrap();
        let key_b64 = base64::engine::general_purpose::STANDARD.encode(kek);
        assert!(!raw.contains(&key_b64));
    }

    #[cfg(unix)]
    #[test]
    fn encrypted_key_file_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.key");
        create_recovery_key(&path, "pass").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    // ── Mnemonic tests ──

    #[test]
    fn mnemonic_roundtrip() {
        let key = random_key_material();
        let phrase = key_to_mnemonic(&key).unwrap();
        let recovered = mnemonic_to_key(&phrase).unwrap();
        assert_eq!(key, recovered);
    }

    #[test]
    fn mnemonic_is_24_words() {
        let key = random_key_material();
        let phrase = key_to_mnemonic(&key).unwrap();
        let word_count = phrase.split_whitespace().count();
        assert_eq!(word_count, 24);
    }

    #[test]
    fn mnemonic_deterministic() {
        let key = [0xABu8; 32];
        let phrase1 = key_to_mnemonic(&key).unwrap();
        let phrase2 = key_to_mnemonic(&key).unwrap();
        assert_eq!(phrase1, phrase2);
    }

    #[test]
    fn different_keys_produce_different_mnemonics() {
        let key1 = [0x01u8; 32];
        let key2 = [0x02u8; 32];
        let phrase1 = key_to_mnemonic(&key1).unwrap();
        let phrase2 = key_to_mnemonic(&key2).unwrap();
        assert_ne!(phrase1, phrase2);
    }

    #[test]
    fn mnemonic_to_key_rejects_invalid_phrase() {
        let result = mnemonic_to_key("not a valid mnemonic phrase");
        assert!(result.is_err());
    }

    #[test]
    fn mnemonic_to_key_rejects_12_word_phrase() {
        // 12-word mnemonic = 16 bytes, not 32
        let short_key = [0x42u8; 16];
        let mnemonic = Mnemonic::from_entropy(&short_key).unwrap();
        let result = mnemonic_to_key(&mnemonic.to_string());
        assert!(result.is_err());
    }

    #[test]
    fn recovery_key_survives_mnemonic_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.key");
        let passphrase = "backup-test";
        let original_key = create_recovery_key(&path, passphrase).unwrap();

        let mnemonic = key_to_mnemonic(&original_key).unwrap();
        let restored_key = mnemonic_to_key(&mnemonic).unwrap();

        assert_eq!(original_key, restored_key);

        // The restored key should also load from disk
        let loaded_key = load_recovery_key(&path, passphrase).unwrap();
        assert_eq!(restored_key, loaded_key);
    }
}
