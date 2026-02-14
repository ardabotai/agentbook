mod error;

#[cfg(target_os = "macos")]
mod macos;

pub use error::SandboxError;

use std::path::{Path, PathBuf};
use tmax_protocol::SandboxConfig;

/// Characters that have special meaning in macOS seatbelt profile syntax.
/// Paths containing any of these must be rejected to prevent profile injection.
const SEATBELT_UNSAFE_CHARS: &[char] = &['"', '(', ')', '\\'];

/// Validate that a canonicalized path does not contain characters that could
/// be used to inject into a seatbelt profile string.
fn validate_path_for_seatbelt(path: &Path) -> Result<(), SandboxError> {
    let s = path.to_string_lossy();
    if let Some(c) = s.chars().find(|c| SEATBELT_UNSAFE_CHARS.contains(c)) {
        return Err(SandboxError::InvalidPath(format!(
            "path contains unsafe character {c:?} for seatbelt profile: {s}"
        )));
    }
    Ok(())
}

/// Resolved sandbox configuration ready to be applied to a child process.
///
/// Created from a `SandboxConfig` by resolving/canonicalizing paths and
/// validating the configuration. The `command_prefix()` method returns the
/// OS-specific command and arguments to wrap the child process.
#[derive(Debug, Clone)]
pub struct ResolvedSandbox {
    /// Canonicalized writable paths.
    pub writable_paths: Vec<PathBuf>,
}

impl ResolvedSandbox {
    /// Resolve a `SandboxConfig` into a `ResolvedSandbox`.
    ///
    /// Canonicalizes all writable paths (following symlinks) and validates
    /// that they exist.
    pub fn resolve(config: &SandboxConfig) -> Result<Self, SandboxError> {
        let mut writable_paths = Vec::with_capacity(config.writable_paths.len());

        for path in &config.writable_paths {
            let resolved = std::fs::canonicalize(path).map_err(|e| {
                SandboxError::InvalidPath(format!("{}: {e}", path.display()))
            })?;
            validate_path_for_seatbelt(&resolved)?;
            writable_paths.push(resolved);
        }

        if writable_paths.is_empty() {
            return Err(SandboxError::InvalidConfig(
                "at least one writable path is required".to_string(),
            ));
        }

        Ok(Self { writable_paths })
    }

    /// Returns the command prefix to wrap a child process in the sandbox.
    ///
    /// On macOS: returns `["sandbox-exec", "-p", "<profile>"]`
    ///
    /// The caller should prepend this to the child command, e.g.:
    /// `sandbox-exec -p "<profile>" /bin/sh -c "..."`.
    pub fn command_prefix(&self) -> Vec<String> {
        #[cfg(target_os = "macos")]
        {
            tracing::warn!(
                "sandbox-exec is deprecated by Apple and may be removed in a future macOS release"
            );
            macos::command_prefix(&self.writable_paths)
        }

        #[cfg(not(target_os = "macos"))]
        {
            tracing::warn!("sandboxing not supported on this platform");
            vec![]
        }
    }
}

/// Validate that a child sandbox's writable paths are subsets of a parent's.
///
/// Each child writable path must be equal to or a subdirectory of at least
/// one parent writable path. Returns `Err(SandboxViolation)` if any child
/// path escapes the parent's scope.
pub fn validate_nesting(
    parent: &ResolvedSandbox,
    child: &ResolvedSandbox,
) -> Result<(), SandboxError> {
    for child_path in &child.writable_paths {
        let is_subset = parent
            .writable_paths
            .iter()
            .any(|parent_path| child_path.starts_with(parent_path));
        if !is_subset {
            return Err(SandboxError::NestingViolation(format!(
                "child path {} is not within any parent writable path",
                child_path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper to create a `ResolvedSandbox` from raw paths (bypasses canonicalization).
    fn resolved_from_paths(paths: Vec<PathBuf>) -> ResolvedSandbox {
        ResolvedSandbox {
            writable_paths: paths,
        }
    }

    #[test]
    fn validate_nesting_subset() {
        let parent = resolved_from_paths(vec![PathBuf::from("/tmp/workspace")]);
        let child = resolved_from_paths(vec![PathBuf::from("/tmp/workspace/subdir")]);
        assert!(validate_nesting(&parent, &child).is_ok());
    }

    #[test]
    fn validate_nesting_exact_match() {
        let parent = resolved_from_paths(vec![PathBuf::from("/tmp/workspace")]);
        let child = resolved_from_paths(vec![PathBuf::from("/tmp/workspace")]);
        assert!(validate_nesting(&parent, &child).is_ok());
    }

    #[test]
    fn validate_nesting_violation() {
        let parent = resolved_from_paths(vec![PathBuf::from("/tmp/workspace")]);
        let child = resolved_from_paths(vec![PathBuf::from("/tmp/other")]);
        assert!(validate_nesting(&parent, &child).is_err());
    }

    #[test]
    fn validate_nesting_multiple_parents() {
        let parent = resolved_from_paths(vec![
            PathBuf::from("/tmp/workspace"),
            PathBuf::from("/home/user"),
        ]);
        let child = resolved_from_paths(vec![
            PathBuf::from("/tmp/workspace/sub"),
            PathBuf::from("/home/user/docs"),
        ]);
        assert!(validate_nesting(&parent, &child).is_ok());
    }

    #[test]
    fn validate_nesting_partial_violation() {
        let parent = resolved_from_paths(vec![PathBuf::from("/tmp/workspace")]);
        let child = resolved_from_paths(vec![
            PathBuf::from("/tmp/workspace/ok"),
            PathBuf::from("/var/escape"),
        ]);
        let result = validate_nesting(&parent, &child);
        assert!(result.is_err());
    }

    #[test]
    fn validate_path_rejects_quote() {
        let path = Path::new("/tmp/evil\"path");
        assert!(validate_path_for_seatbelt(path).is_err());
    }

    #[test]
    fn validate_path_rejects_paren() {
        let path = Path::new("/tmp/evil(path");
        assert!(validate_path_for_seatbelt(path).is_err());
    }

    #[test]
    fn validate_path_accepts_normal() {
        let path = Path::new("/tmp/normal-path_123/foo.bar");
        assert!(validate_path_for_seatbelt(path).is_ok());
    }

    #[test]
    fn resolve_requires_writable_paths() {
        let config = SandboxConfig {
            writable_paths: vec![],
        };
        assert!(ResolvedSandbox::resolve(&config).is_err());
    }

    #[test]
    fn resolve_canonicalizes_paths() {
        let dir = tempfile::tempdir().unwrap();
        let config = SandboxConfig {
            writable_paths: vec![dir.path().to_path_buf()],
        };
        let resolved = ResolvedSandbox::resolve(&config).unwrap();
        // Canonicalized path should not contain symlinks
        assert!(resolved.writable_paths[0].is_absolute());
        // On macOS, /tmp -> /private/tmp
        #[cfg(target_os = "macos")]
        {
            let canonical = std::fs::canonicalize(dir.path()).unwrap();
            assert_eq!(resolved.writable_paths[0], canonical);
        }
    }
}
