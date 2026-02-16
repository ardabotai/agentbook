use agentbook_crypto::crypto::evm_address_from_public_key;
use anyhow::{Context, Result};
use k256::SecretKey;
use rand::rngs::OsRng;
use std::path::Path;

const YOLO_KEY_FILE: &str = "yolo.key";

/// Generate a new yolo wallet key and save it to state_dir/yolo.key.
/// The key is stored in plaintext — the whole point of yolo mode is no auth.
pub fn generate_yolo_key(state_dir: &Path) -> Result<[u8; 32]> {
    let secret = SecretKey::random(&mut OsRng);
    let key_bytes: [u8; 32] = secret.to_bytes().into();

    let path = state_dir.join(YOLO_KEY_FILE);
    let hex_str = hex::encode(key_bytes);
    std::fs::write(&path, hex_str)
        .with_context(|| format!("failed to write {}", path.display()))?;

    // Set restrictive permissions even though it's a hot wallet
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("failed to set yolo.key permissions")?;
    }

    Ok(key_bytes)
}

/// Load the yolo wallet key from state_dir/yolo.key.
pub fn load_yolo_key(state_dir: &Path) -> Result<[u8; 32]> {
    let path = state_dir.join(YOLO_KEY_FILE);
    let hex_str = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let hex_str = hex_str.trim();

    let bytes = hex::decode(hex_str).context("invalid hex in yolo.key")?;
    if bytes.len() != 32 {
        anyhow::bail!(
            "yolo.key has invalid length: expected 32 bytes, got {}",
            bytes.len()
        );
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Check whether a yolo key file exists.
pub fn has_yolo_key(state_dir: &Path) -> bool {
    state_dir.join(YOLO_KEY_FILE).exists()
}

/// Load or generate the yolo key, then derive its EVM address.
pub fn yolo_address(state_dir: &Path) -> Result<String> {
    let key_bytes = if has_yolo_key(state_dir) {
        load_yolo_key(state_dir)?
    } else {
        generate_yolo_key(state_dir)?
    };

    let secret = SecretKey::from_slice(&key_bytes).context("invalid yolo key bytes")?;
    Ok(evm_address_from_public_key(&secret.public_key()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_load_yolo_key() {
        let dir = tempfile::tempdir().unwrap();

        assert!(!has_yolo_key(dir.path()));

        let generated = generate_yolo_key(dir.path()).unwrap();
        assert!(has_yolo_key(dir.path()));

        let loaded = load_yolo_key(dir.path()).unwrap();
        assert_eq!(generated, loaded);
    }

    #[test]
    fn yolo_address_creates_key_if_missing() {
        let dir = tempfile::tempdir().unwrap();

        let addr = yolo_address(dir.path()).unwrap();
        assert!(addr.starts_with("0x"));
        assert_eq!(addr.len(), 42);
        assert!(has_yolo_key(dir.path()));
    }

    #[test]
    fn yolo_address_deterministic() {
        let dir = tempfile::tempdir().unwrap();

        let addr1 = yolo_address(dir.path()).unwrap();
        let addr2 = yolo_address(dir.path()).unwrap();
        assert_eq!(addr1, addr2);
    }

    #[test]
    fn yolo_key_is_32_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let key = generate_yolo_key(dir.path()).unwrap();
        assert_eq!(key.len(), 32);
        // Verify it's a valid secret key
        assert!(SecretKey::from_slice(&key).is_ok());
    }
}
