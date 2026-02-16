use anyhow::{Context, Result};
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const DEFAULT_STATE_DIR: &str = ".local/state/agentbook";

/// Return the agentbook state directory path.
///
/// Priority: `$AGENTBOOK_STATE_DIR` env var, then `~/.local/state/agentbook`.
pub fn default_state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("AGENTBOOK_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").context("HOME env var not set")?;
    Ok(PathBuf::from(home).join(DEFAULT_STATE_DIR))
}

/// Ensure the state directory exists with `0700` permissions.
pub fn ensure_state_dir(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create state dir {}", path.display()))?;
    }
    #[cfg(unix)]
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to set state dir permissions {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_creates_dir() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("sub/state");
        ensure_state_dir(&state).unwrap();
        assert!(state.exists());
    }

    #[cfg(unix)]
    #[test]
    fn state_dir_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("perms");
        ensure_state_dir(&state).unwrap();
        let meta = std::fs::metadata(&state).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }
}
