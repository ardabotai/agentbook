mod error;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

pub use error::SandboxError;

use std::path::PathBuf;
use tmax_protocol::SandboxConfig;

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
    /// On Linux: returns `[]` (stub — not yet implemented)
    ///
    /// The caller should prepend this to the child command, e.g.:
    /// `sandbox-exec -p "<profile>" /bin/sh -c "..."`.
    pub fn command_prefix(&self) -> Vec<String> {
        #[cfg(target_os = "macos")]
        {
            macos::command_prefix(&self.writable_paths)
        }

        #[cfg(target_os = "linux")]
        {
            linux::command_prefix(&self.writable_paths)
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            tracing::warn!("sandboxing not supported on this platform");
            vec![]
        }
    }
}

/// Validate that `child_paths` are subsets of `parent_paths`.
///
/// Each child writable path must be equal to or a subdirectory of at least
/// one parent writable path. Returns `Err(SandboxViolation)` if any child
/// path escapes the parent's scope.
pub fn validate_nesting(
    parent_paths: &[PathBuf],
    child_paths: &[PathBuf],
) -> Result<(), SandboxError> {
    for child in child_paths {
        let is_subset = parent_paths.iter().any(|parent| child.starts_with(parent));
        if !is_subset {
            return Err(SandboxError::NestingViolation(format!(
                "child path {} is not within any parent writable path",
                child.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn validate_nesting_subset() {
        let parent = vec![PathBuf::from("/tmp/workspace")];
        let child = vec![PathBuf::from("/tmp/workspace/subdir")];
        assert!(validate_nesting(&parent, &child).is_ok());
    }

    #[test]
    fn validate_nesting_exact_match() {
        let parent = vec![PathBuf::from("/tmp/workspace")];
        let child = vec![PathBuf::from("/tmp/workspace")];
        assert!(validate_nesting(&parent, &child).is_ok());
    }

    #[test]
    fn validate_nesting_violation() {
        let parent = vec![PathBuf::from("/tmp/workspace")];
        let child = vec![PathBuf::from("/tmp/other")];
        assert!(validate_nesting(&parent, &child).is_err());
    }

    #[test]
    fn validate_nesting_multiple_parents() {
        let parent = vec![
            PathBuf::from("/tmp/workspace"),
            PathBuf::from("/home/user"),
        ];
        let child = vec![
            PathBuf::from("/tmp/workspace/sub"),
            PathBuf::from("/home/user/docs"),
        ];
        assert!(validate_nesting(&parent, &child).is_ok());
    }

    #[test]
    fn validate_nesting_partial_violation() {
        let parent = vec![PathBuf::from("/tmp/workspace")];
        let child = vec![
            PathBuf::from("/tmp/workspace/ok"),
            PathBuf::from("/var/escape"),
        ];
        let result = validate_nesting(&parent, &child);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_requires_writable_paths() {
        let config = SandboxConfig {
            writable_paths: vec![],
            inherit_parent: false,
        };
        assert!(ResolvedSandbox::resolve(&config).is_err());
    }

    #[test]
    fn resolve_canonicalizes_paths() {
        let dir = tempfile::tempdir().unwrap();
        let config = SandboxConfig {
            writable_paths: vec![dir.path().to_path_buf()],
            inherit_parent: false,
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
