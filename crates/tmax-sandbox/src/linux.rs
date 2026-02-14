use std::path::PathBuf;

/// Returns the command prefix for Linux sandboxing.
///
/// Stub implementation — Linux namespace sandboxing (unshare + mount)
/// will be implemented in a future phase.
pub fn command_prefix(_writable_paths: &[PathBuf]) -> Vec<String> {
    tracing::warn!("Linux sandboxing not yet implemented");
    vec![]
}
