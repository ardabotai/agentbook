use std::path::PathBuf;

/// Generate a sandbox-exec seatbelt profile string.
///
/// The profile:
/// 1. Allows all operations by default, except file-write and network
/// 2. Denies all file-write operations
/// 3. Denies all network operations (prevents data exfiltration)
/// 4. Selectively allows writes to each specified path (and subdirectories)
/// 5. Allows writes to specific /dev entries (null, zero, TTY, PTY devices)
fn generate_profile(writable_paths: &[PathBuf]) -> String {
    let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write*)\n(deny network*)\n");

    // Allow writes to specific /dev entries needed for I/O
    profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");
    profile.push_str("(allow file-write* (literal \"/dev/zero\"))\n");
    profile.push_str("(allow file-write* (regex #\"^/dev/ttys[0-9]+$\"))\n");
    profile.push_str("(allow file-write* (regex #\"^/dev/pty[a-z][0-9a-f]$\"))\n");

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
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(subpath \"/private/tmp/workspace\")"));
        assert!(profile.contains("(subpath \"/Users/test/project\")"));
        assert!(profile.contains("(literal \"/dev/null\")"));
        assert!(profile.contains("(literal \"/dev/zero\")"));
        assert!(profile.contains("(regex #\"^/dev/ttys[0-9]+$\")"));
        assert!(profile.contains("(regex #\"^/dev/pty[a-z][0-9a-f]$\")"));
        assert!(!profile.contains("(subpath \"/dev\")"));
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
