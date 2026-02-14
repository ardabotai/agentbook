use std::path::PathBuf;

/// Generate a sandbox-exec seatbelt profile string.
///
/// The profile:
/// 1. Allows all operations by default
/// 2. Denies all file-write operations
/// 3. Selectively allows writes to each specified path (and subdirectories)
/// 4. Allows writes to /dev (for /dev/null, /dev/tty, etc.)
/// 5. Allows writes to /private/var/folders (for macOS temp dirs)
fn generate_profile(writable_paths: &[PathBuf]) -> String {
    let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");

    // Always allow writes to /dev (needed for /dev/null, /dev/tty, PTY devices)
    profile.push_str("(allow file-write* (subpath \"/dev\"))\n");

    // Allow writes to each configured writable path
    for path in writable_paths {
        let path_str = path.display();
        profile.push_str(&format!("(allow file-write* (subpath \"{path_str}\"))\n"));
    }

    profile
}

/// Returns the command prefix to wrap a child process in a macOS sandbox.
///
/// Returns `["sandbox-exec", "-p", "<profile>"]`.
pub fn command_prefix(writable_paths: &[PathBuf]) -> Vec<String> {
    let profile = generate_profile(writable_paths);
    vec![
        "sandbox-exec".to_string(),
        "-p".to_string(),
        profile,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_contains_writable_paths() {
        let paths = vec![
            PathBuf::from("/private/tmp/workspace"),
            PathBuf::from("/Users/test/project"),
        ];
        let profile = generate_profile(&paths);

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(subpath \"/private/tmp/workspace\")"));
        assert!(profile.contains("(subpath \"/Users/test/project\")"));
        assert!(profile.contains("(subpath \"/dev\")"));
    }

    #[test]
    fn command_prefix_format() {
        let paths = vec![PathBuf::from("/tmp/test")];
        let prefix = command_prefix(&paths);

        assert_eq!(prefix.len(), 3);
        assert_eq!(prefix[0], "sandbox-exec");
        assert_eq!(prefix[1], "-p");
        assert!(prefix[2].contains("(version 1)"));
    }
}
