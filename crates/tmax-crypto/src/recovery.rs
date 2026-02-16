use crate::crypto::{ENVELOPE_KEY_BYTES, random_key_material};
use anyhow::{Context, Result, bail};
use base64::Engine;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Load an existing recovery key or create a new one at the given path.
///
/// If `path` is `None`, generates an ephemeral key (not persisted).
pub fn load_or_create_recovery_key(path: Option<&Path>) -> Result<[u8; ENVELOPE_KEY_BYTES]> {
    let Some(path) = path else {
        return Ok(random_key_material());
    };

    if path.exists() {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read recovery key {}", path.display()))?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw.trim())
            .context("recovery key is not valid base64")?;
        if decoded.len() != ENVELOPE_KEY_BYTES {
            bail!("recovery key must be {ENVELOPE_KEY_BYTES} bytes");
        }
        let mut key = [0u8; ENVELOPE_KEY_BYTES];
        key.copy_from_slice(&decoded);
        return Ok(key);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create recovery key directory {}",
                parent.display()
            )
        })?;
    }

    let key = random_key_material();
    let encoded = base64::engine::general_purpose::STANDARD.encode(key);
    fs::write(path, encoded)
        .with_context(|| format!("failed to write recovery key {}", path.display()))?;
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!("failed to set recovery key permissions {}", path.display())
        })?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_key_is_random() {
        let k1 = load_or_create_recovery_key(None).unwrap();
        let k2 = load_or_create_recovery_key(None).unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn create_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.key");
        let created = load_or_create_recovery_key(Some(&path)).unwrap();
        let loaded = load_or_create_recovery_key(Some(&path)).unwrap();
        assert_eq!(created, loaded);
    }

    #[cfg(unix)]
    #[test]
    fn key_file_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.key");
        let _ = load_or_create_recovery_key(Some(&path)).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
