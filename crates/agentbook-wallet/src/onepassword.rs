//! 1Password CLI (`op`) integration for seamless biometric-backed secrets.
//!
//! When the `op` CLI is installed and authenticated, agentbook can:
//! - Auto-fill the passphrase on node startup (no typing)
//! - Auto-generate TOTP codes for restricted actions (biometric confirmation)
//! - Store all secrets (passphrase, mnemonic, TOTP, yolo mnemonic) in a single item
//!
//! The 1Password item is named `agentbook-XXXX` where XXXX is the last 4 hex characters
//! of the node's EVM address (node_id), allowing multiple identities on one machine.
//!
//! Falls back gracefully to manual prompts when `op` is not available.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// Build the 1Password item title from the last 4 chars of the node_id (EVM address).
///
/// `node_id` is the full EVM address string (e.g. `"0x1a2B...cD3e"`).
/// Returns e.g. `"agentbook-cD3e"`.
pub fn op_item_title(node_id: &str) -> String {
    let suffix = if node_id.len() >= 4 {
        &node_id[node_id.len() - 4..]
    } else {
        node_id
    };
    format!("agentbook-{suffix}")
}

/// Read the node_id from `node.json` in the state dir and derive the OP item title.
///
/// Returns `None` if `node.json` doesn't exist or can't be parsed.
pub fn item_title_from_state_dir(state_dir: &Path) -> Option<String> {
    let meta_path = state_dir.join("node.json");
    let meta_str = std::fs::read_to_string(meta_path).ok()?;
    let meta: serde_json::Value = serde_json::from_str(&meta_str).ok()?;
    let node_id = meta.get("node_id")?.as_str()?;
    if node_id.is_empty() {
        return None;
    }
    Some(op_item_title(node_id))
}

/// Check whether the 1Password CLI (`op`) is installed and responsive.
pub fn has_op_cli() -> bool {
    Command::new("op")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create the unified 1Password item with passphrase, mnemonic, and TOTP.
///
/// The item is a Login with fields for passphrase, recovery mnemonic, and TOTP.
/// The ETH address is stored in the notes field for easy identification.
pub fn save_agentbook_item(
    title: &str,
    node_id: &str,
    passphrase: &str,
    mnemonic: &str,
    otpauth_url: &str,
) -> Result<()> {
    let notes = format!("ETH address: {node_id}");
    let status = Command::new("op")
        .args([
            "item",
            "create",
            "--category",
            "Login",
            "--title",
            title,
            &format!("passphrase[password]={passphrase}"),
            &format!("recovery_mnemonic[password]={mnemonic}"),
            &format!("totp[otp]={otpauth_url}"),
            &format!("notesPlain[text]={notes}"),
            "--tags",
            "agentbook,crypto",
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .context("failed to run `op item create`")?;

    if !status.success() {
        anyhow::bail!("op item create failed (exit {})", status);
    }
    Ok(())
}

/// Create a separate 1Password item for the yolo wallet.
///
/// `yolo_address` is the full EVM address of the yolo wallet.
/// The item title is `agentbook-XXXX-yolo` (last 4 of address + `-yolo`).
/// The ETH address is stored in the notes field.
pub fn save_yolo_item(yolo_address: &str, mnemonic: &str) -> Result<()> {
    let title = format!("{}-yolo", op_item_title(yolo_address));
    let notes = format!("ETH address: {yolo_address}");
    let status = Command::new("op")
        .args([
            "item",
            "create",
            "--category",
            "Login",
            "--title",
            &title,
            &format!("yolo_mnemonic[password]={mnemonic}"),
            &format!("notesPlain[text]={notes}"),
            "--tags",
            "agentbook,crypto,yolo",
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .context("failed to run `op item create` for yolo")?;

    if !status.success() {
        anyhow::bail!("op item create (yolo) failed (exit {})", status);
    }
    Ok(())
}

/// Read the passphrase from 1Password.
///
/// Returns `Ok(passphrase)` on success, or an error if `op` fails
/// (e.g. biometric denied, item not found, CLI not authenticated).
pub fn read_passphrase(title: &str) -> Result<String> {
    let output = Command::new("op")
        .args(["read", &format!("op://Personal/{title}/passphrase")])
        .stderr(std::process::Stdio::piped())
        .output()
        .context("failed to run `op read`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("op read passphrase failed: {stderr}");
    }

    Ok(String::from_utf8(output.stdout)
        .context("op returned non-UTF8 passphrase")?
        .trim()
        .to_string())
}

/// Read a TOTP code from 1Password (triggers biometric).
///
/// Returns a 6-digit OTP string.
pub fn read_otp(title: &str) -> Result<String> {
    let output = Command::new("op")
        .args(["item", "get", title, "--otp"])
        .stderr(std::process::Stdio::piped())
        .output()
        .context("failed to run `op item get --otp`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("op item get --otp failed: {stderr}");
    }

    Ok(String::from_utf8(output.stdout)
        .context("op returned non-UTF8 OTP")?
        .trim()
        .to_string())
}

/// Check whether the agentbook item already exists in 1Password.
pub fn has_agentbook_item(title: &str) -> bool {
    Command::new("op")
        .args(["item", "get", title, "--format", "json"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_op_cli_does_not_panic() {
        // Just verify it runs without panicking — result depends on environment
        let _ = has_op_cli();
    }

    #[test]
    fn has_agentbook_item_does_not_panic() {
        let _ = has_agentbook_item("agentbook-test");
    }

    #[test]
    fn op_item_title_uses_last_4_chars() {
        assert_eq!(
            op_item_title("0x1a2b3c4d5e6f7890abcdef1234567890abcDEF12"),
            "agentbook-EF12"
        );
    }

    #[test]
    fn op_item_title_short_input() {
        assert_eq!(op_item_title("AB"), "agentbook-AB");
    }

    #[test]
    fn op_item_title_exact_4_chars() {
        assert_eq!(op_item_title("cD3e"), "agentbook-cD3e");
    }

    #[test]
    fn item_title_from_state_dir_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(item_title_from_state_dir(dir.path()).is_none());
    }

    #[test]
    fn item_title_from_state_dir_reads_node_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("node.json"),
            r#"{"node_id":"0xAbCd1234567890abcdef1234567890abcdef5678","public_key_b64":"xxx","created_at_ms":0}"#,
        )
        .unwrap();
        assert_eq!(
            item_title_from_state_dir(dir.path()),
            Some("agentbook-5678".to_string())
        );
    }
}
