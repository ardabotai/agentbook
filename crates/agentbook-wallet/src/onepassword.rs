//! 1Password CLI (`op`) integration for seamless biometric-backed secrets.
//!
//! When the `op` CLI is installed and authenticated, agentbook can:
//! - Auto-fill the passphrase on node startup (no typing)
//! - Auto-generate TOTP codes for restricted actions (biometric confirmation)
//! - Store all secrets (passphrase, mnemonic, TOTP, yolo mnemonic) in a single item
//!
//! Falls back gracefully to manual prompts when `op` is not available.

use anyhow::{Context, Result};
use std::process::Command;

/// The 1Password item title used for all agentbook secrets.
const OP_ITEM_TITLE: &str = "agentbook";

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
/// The item is a Login with three fields:
/// - `passphrase` (password) — recovery key passphrase
/// - `recovery_mnemonic` (password) — 24-word BIP-39 backup
/// - `totp` (otp) — TOTP secret as otpauth:// URL
pub fn save_agentbook_item(passphrase: &str, mnemonic: &str, otpauth_url: &str) -> Result<()> {
    let status = Command::new("op")
        .args([
            "item",
            "create",
            "--category",
            "Login",
            "--title",
            OP_ITEM_TITLE,
            &format!("passphrase[password]={passphrase}"),
            &format!("recovery_mnemonic[password]={mnemonic}"),
            &format!("totp[otp]={otpauth_url}"),
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

/// Add or update the yolo wallet mnemonic on the existing 1Password item.
pub fn save_yolo_mnemonic(mnemonic: &str) -> Result<()> {
    let status = Command::new("op")
        .args([
            "item",
            "edit",
            OP_ITEM_TITLE,
            &format!("yolo_mnemonic[password]={mnemonic}"),
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .context("failed to run `op item edit`")?;

    if !status.success() {
        anyhow::bail!("op item edit failed (exit {})", status);
    }
    Ok(())
}

/// Read the passphrase from 1Password.
///
/// Returns `Ok(passphrase)` on success, or an error if `op` fails
/// (e.g. biometric denied, item not found, CLI not authenticated).
pub fn read_passphrase() -> Result<String> {
    let output = Command::new("op")
        .args(["read", &format!("op://Personal/{OP_ITEM_TITLE}/passphrase")])
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
pub fn read_otp() -> Result<String> {
    let output = Command::new("op")
        .args(["item", "get", OP_ITEM_TITLE, "--otp"])
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

/// Check whether the "agentbook" item already exists in 1Password.
pub fn has_agentbook_item() -> bool {
    Command::new("op")
        .args(["item", "get", OP_ITEM_TITLE, "--format", "json"])
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
        let _ = has_agentbook_item();
    }
}
