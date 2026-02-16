use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use tmax_protocol::SandboxConfig;

#[cfg(target_os = "macos")]
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStrategy {
    Disabled,
    LinuxNamespaces,
    MacOsSandboxExec,
    Unsupported,
}

pub fn current_strategy() -> SandboxStrategy {
    #[cfg(target_os = "linux")]
    {
        return SandboxStrategy::LinuxNamespaces;
    }

    #[cfg(target_os = "macos")]
    {
        return SandboxStrategy::MacOsSandboxExec;
    }

    #[allow(unreachable_code)]
    SandboxStrategy::Unsupported
}

pub fn sandboxed_spawn_command(
    exec: &str,
    args: &[String],
    config: Option<&SandboxConfig>,
) -> Result<(String, Vec<String>)> {
    let Some(config) = config else {
        return Ok((exec.to_string(), args.to_vec()));
    };

    #[cfg(target_os = "linux")]
    {
        linux_namespace_spawn_command(exec, args, config)
    }

    #[cfg(target_os = "macos")]
    {
        maybe_warn_macos_interim_sandbox();
        macos_sandbox_exec_command(exec, args, config)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        bail!("sandboxed sessions are not supported on this platform")
    }
}

pub fn effective_child_scope(
    parent: Option<&SandboxConfig>,
    requested: Option<&SandboxConfig>,
    cwd: &Path,
) -> Result<Option<SandboxConfig>> {
    let parent_normalized = match parent {
        Some(parent) => Some(normalize_scope(parent, cwd)?),
        None => None,
    };

    let requested_normalized = match requested {
        Some(requested) => Some(normalize_scope(requested, cwd)?),
        None => None,
    };

    match (parent_normalized, requested_normalized) {
        (None, None) => Ok(None),
        (None, Some(child)) => Ok(Some(child)),
        (Some(parent), None) => Ok(Some(parent)),
        (Some(parent), Some(child)) => {
            validate_child_subset(&parent, &child)?;
            Ok(Some(child))
        }
    }
}

pub fn normalize_scope(config: &SandboxConfig, cwd: &Path) -> Result<SandboxConfig> {
    let mut writable = Vec::with_capacity(config.writable_paths.len());
    for path in &config.writable_paths {
        writable.push(resolve_path(path, cwd)?);
    }
    let mut readable = Vec::with_capacity(config.readable_paths.len());
    for path in &config.readable_paths {
        readable.push(resolve_path(path, cwd)?);
    }
    Ok(SandboxConfig {
        writable_paths: dedup_paths(writable),
        readable_paths: dedup_paths(readable),
    })
}

pub fn validate_child_subset(parent: &SandboxConfig, child: &SandboxConfig) -> Result<()> {
    for child_path in &child.writable_paths {
        let allowed = parent
            .writable_paths
            .iter()
            .any(|parent_path| is_path_within(child_path, parent_path));
        if !allowed {
            bail!(
                "child sandbox writable path '{}' is outside parent scope",
                child_path.display()
            );
        }
    }
    // readable_paths must also be within parent's readable or writable scope
    let parent_all_readable: Vec<&Path> = parent
        .readable_paths
        .iter()
        .chain(parent.writable_paths.iter())
        .map(|p| p.as_path())
        .collect();
    for child_path in &child.readable_paths {
        let allowed = parent_all_readable
            .iter()
            .any(|parent_path| is_path_within(child_path, parent_path));
        if !allowed {
            bail!(
                "child sandbox readable path '{}' is outside parent scope",
                child_path.display()
            );
        }
    }
    Ok(())
}

pub fn is_path_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn resolve_path(path: &Path, cwd: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    let normalized = normalize_absolute_path(&abs)?;
    canonicalize_with_missing_tail(&normalized)
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!(
            "sandbox path must resolve to absolute path: {}",
            path.display()
        );
    }

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
                if out.as_os_str().is_empty() {
                    out.push(Path::new("/"));
                }
            }
            Component::Normal(seg) => out.push(seg),
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
        }
    }

    if out.as_os_str().is_empty() {
        bail!("failed to normalize sandbox path: {}", path.display());
    }

    Ok(out)
}

fn dedup_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf> {
    let mut missing_components = Vec::new();
    let mut probe = path.to_path_buf();

    while !probe.exists() {
        let Some(name) = probe.file_name() else {
            break;
        };
        missing_components.push(name.to_os_string());
        let Some(parent) = probe.parent() else {
            break;
        };
        probe = parent.to_path_buf();
    }

    let mut canonical = if probe.exists() {
        probe.canonicalize()?
    } else {
        path.to_path_buf()
    };

    for component in missing_components.iter().rev() {
        canonical.push(component);
    }

    Ok(canonical)
}

#[cfg(target_os = "macos")]
fn macos_sandbox_exec_command(
    exec: &str,
    args: &[String],
    config: &SandboxConfig,
) -> Result<(String, Vec<String>)> {
    let sandbox_exec = Path::new("/usr/bin/sandbox-exec");
    if !sandbox_exec.exists() {
        bail!("sandbox-exec not available on this macOS host");
    }

    let profile = build_macos_sandbox_profile(config)?;
    let mut argv = vec!["-p".to_string(), profile, exec.to_string()];
    argv.extend(args.iter().cloned());
    Ok((sandbox_exec.display().to_string(), argv))
}

#[cfg(target_os = "macos")]
fn maybe_warn_macos_interim_sandbox() {
    static WARNED: OnceLock<()> = OnceLock::new();
    if WARNED.set(()).is_ok() && !macos_containerization_supported() {
        tracing::warn!(
            "macOS Containerization API is unavailable; using sandbox-exec interim sandbox backend"
        );
    }
}

#[cfg(target_os = "linux")]
fn linux_namespace_spawn_command(
    exec: &str,
    args: &[String],
    config: &SandboxConfig,
) -> Result<(String, Vec<String>)> {
    let runner = resolve_linux_runner_path();
    let mut argv = Vec::new();
    for path in &config.readable_paths {
        argv.push("--readable".to_string());
        argv.push(path.display().to_string());
    }
    for path in &config.writable_paths {
        argv.push("--writable".to_string());
        argv.push(path.display().to_string());
    }
    argv.push("--exec".to_string());
    argv.push(exec.to_string());
    argv.push("--".to_string());
    argv.extend(args.iter().cloned());
    Ok((runner, argv))
}

#[cfg(target_os = "linux")]
fn resolve_linux_runner_path() -> String {
    if let Ok(path) = std::env::var("TMAX_SANDBOX_RUNNER") {
        return path;
    }

    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        let sibling = parent.join("tmax-sandbox-runner");
        if sibling.exists() {
            return sibling.display().to_string();
        }
    }

    "tmax-sandbox-runner".to_string()
}

#[cfg(target_os = "macos")]
fn build_macos_sandbox_profile(config: &SandboxConfig) -> Result<String> {
    let mut lines = vec![
        "(version 1)".to_string(),
        "(deny default)".to_string(),
        // Process management
        "(allow process-exec*)".to_string(),
        "(allow process-fork)".to_string(),
        "(allow sysctl-read)".to_string(),
        "(allow mach-lookup)".to_string(),
        "(allow signal)".to_string(),
        // Root directory entry (needed for path traversal)
        "(allow file-read* (literal \"/\"))".to_string(),
        // System paths (read-only) — binaries, libraries, frameworks
        "(allow file-read* (subpath \"/usr\"))".to_string(),
        "(allow file-read* (subpath \"/bin\"))".to_string(),
        "(allow file-read* (subpath \"/sbin\"))".to_string(),
        "(allow file-read* (subpath \"/System\"))".to_string(),
        "(allow file-read* (subpath \"/Library\"))".to_string(),
        "(allow file-read* (subpath \"/dev\"))".to_string(),
        // /private/etc for shell config, /private/var/db for system state
        "(allow file-read* (subpath \"/private/etc\"))".to_string(),
        "(allow file-read* (subpath \"/private/var/db\"))".to_string(),
        "(allow file-read* (subpath \"/private/var/run\"))".to_string(),
        // Symlink targets: /etc→/private/etc, /var→/private/var, /tmp→/private/tmp
        "(allow file-read* (subpath \"/etc\"))".to_string(),
        "(allow file-read* (subpath \"/var/db\"))".to_string(),
        "(allow file-read* (subpath \"/var/run\"))".to_string(),
        // Device writes (stdio, /dev/null, /dev/tty)
        "(allow file-write* (subpath \"/dev\"))".to_string(),
    ];

    // Collect all paths that need sandbox access
    let mut all_paths: Vec<PathBuf> = Vec::new();

    // Read-only paths
    for path in &config.readable_paths {
        let canonical = canonicalize_with_missing_tail(path)?;
        let escaped = escape_profile_path(canonical.as_os_str());
        lines.push(format!("(allow file-read* (subpath \"{escaped}\"))"));
        all_paths.push(canonical.clone());
        if let Some(alt) = macos_symlink_alt(&canonical) {
            let alt_escaped = escape_profile_path(alt.as_os_str());
            lines.push(format!("(allow file-read* (subpath \"{alt_escaped}\"))"));
            all_paths.push(alt);
        }
    }

    // Writable paths (implicitly grants read too)
    for path in &config.writable_paths {
        let canonical = canonicalize_with_missing_tail(path)?;
        let escaped = escape_profile_path(canonical.as_os_str());
        lines.push(format!("(allow file-read* (subpath \"{escaped}\"))"));
        lines.push(format!("(allow file-write* (subpath \"{escaped}\"))"));
        all_paths.push(canonical.clone());
        if let Some(alt) = macos_symlink_alt(&canonical) {
            let alt_escaped = escape_profile_path(alt.as_os_str());
            lines.push(format!("(allow file-read* (subpath \"{alt_escaped}\"))"));
            lines.push(format!("(allow file-write* (subpath \"{alt_escaped}\"))"));
            all_paths.push(alt);
        }
    }

    // Allow reading ancestor directories so path traversal works
    let mut ancestors: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for p in &all_paths {
        let mut current = p.parent();
        while let Some(dir) = current {
            if dir == Path::new("/") {
                break;
            }
            if !ancestors.insert(dir.to_path_buf()) {
                break; // already seen this path and its parents
            }
            current = dir.parent();
        }
    }
    for ancestor in &ancestors {
        let escaped = escape_profile_path(ancestor.as_os_str());
        lines.push(format!("(allow file-read* (literal \"{escaped}\"))"));
    }

    Ok(lines.join("\n"))
}

/// On macOS, /var → /private/var, /etc → /private/etc, /tmp → /private/tmp.
/// When a canonical path starts with /private/var (etc.), return the /var (etc.) variant
/// so the sandbox profile covers both symlinked and canonical access.
#[cfg(target_os = "macos")]
fn macos_symlink_alt(canonical: &Path) -> Option<PathBuf> {
    let s = canonical.to_str()?;
    // Canonical → symlink: /private/var/... → /var/...
    for prefix in &["/private/var/", "/private/etc/", "/private/tmp/"] {
        if let Some(rest) = s.strip_prefix("/private")
            && s.starts_with(prefix)
        {
            return Some(PathBuf::from(rest));
        }
    }
    // Symlink → canonical (shouldn't happen after canonicalize, but be safe)
    for prefix in &["/var/", "/etc/", "/tmp/"] {
        if s.starts_with(prefix) {
            return Some(PathBuf::from(format!("/private{s}")));
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn escape_profile_path(path: &OsStr) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
pub fn macos_containerization_supported() -> bool {
    use std::process::Command;

    let output = match Command::new("sw_vers").arg("-productVersion").output() {
        Ok(output) => output,
        Err(_) => return false,
    };

    if !output.status.success() {
        return false;
    }

    let version = String::from_utf8_lossy(&output.stdout);
    let major = version
        .trim()
        .split('.')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    major >= 26
}

#[cfg(not(target_os = "macos"))]
pub fn macos_containerization_supported() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cfg(paths: &[&str]) -> SandboxConfig {
        SandboxConfig {
            writable_paths: paths.iter().map(PathBuf::from).collect(),
            readable_paths: vec![],
        }
    }

    #[test]
    fn normalizes_relative_paths_against_cwd() {
        let cwd = Path::new("/tmp/repo");
        let scope = normalize_scope(&cfg(&["a/./b", "../repo/c"]), cwd).expect("normalize");
        assert!(scope.writable_paths[0].ends_with(Path::new("tmp/repo/a/b")));
        assert!(scope.writable_paths[1].ends_with(Path::new("tmp/repo/c")));
    }

    #[test]
    fn child_subset_is_allowed() {
        let parent = cfg(&["/workspace"]);
        let child = cfg(&["/workspace/project"]);
        validate_child_subset(&parent, &child).expect("subset should pass");
    }

    #[test]
    fn child_outside_parent_is_rejected() {
        let parent = cfg(&["/workspace"]);
        let child = cfg(&["/tmp"]);
        let err = validate_child_subset(&parent, &child).expect_err("must reject");
        assert!(err.to_string().contains("outside parent scope"));
    }

    #[test]
    fn effective_scope_inherits_parent_when_child_missing() {
        let cwd = Path::new("/");
        let parent = cfg(&["/workspace"]);
        let eff = effective_child_scope(Some(&parent), None, cwd).expect("effective scope");
        assert!(eff.is_some());
        let eff = eff.expect("scope must exist");
        assert_eq!(eff.writable_paths, vec![PathBuf::from("/workspace")]);
    }

    #[test]
    fn effective_scope_none_when_both_missing() {
        let cwd = Path::new("/");
        let eff = effective_child_scope(None, None, cwd).expect("effective scope");
        assert!(eff.is_none());
    }

    #[test]
    fn unsandboxed_spawn_returns_original_command() {
        let (exec, args) =
            sandboxed_spawn_command("echo", &["hello".to_string()], None).expect("spawn command");
        assert_eq!(exec, "echo");
        assert_eq!(args, vec!["hello".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_by_subset_validation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).expect("mkdir root");
        fs::create_dir_all(&outside).expect("mkdir outside");

        std::os::unix::fs::symlink(&outside, root.join("link")).expect("symlink");

        let parent = normalize_scope(
            &SandboxConfig {
                writable_paths: vec![root.clone()],
                readable_paths: vec![],
            },
            Path::new("/"),
        )
        .expect("normalize parent");
        let child = normalize_scope(
            &SandboxConfig {
                writable_paths: vec![root.join("link")],
                readable_paths: vec![],
            },
            Path::new("/"),
        )
        .expect("normalize child");

        let err = validate_child_subset(&parent, &child).expect_err("must reject symlink escape");
        assert!(err.to_string().contains("outside parent scope"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_denies_outside_write_and_allows_inside() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("tempdir");
        let inside = temp.path().join("inside");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&inside).expect("mkdir inside");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        let inside_file = inside.join("ok.txt");
        let outside_file = outside.join("blocked.txt");

        let cfg = SandboxConfig {
            writable_paths: vec![inside.clone()],
            readable_paths: vec![],
        };
        let cfg = normalize_scope(&cfg, Path::new("/")).expect("normalize");
        let (program, argv) = sandboxed_spawn_command(
            "/bin/sh",
            &[
                "-lc".to_string(),
                format!(
                    "echo ok > \"{}\"; echo blocked > \"{}\"",
                    inside_file.display(),
                    outside_file.display()
                ),
            ],
            Some(&cfg),
        )
        .expect("spawn wrapper");

        let output = Command::new(program).args(argv).output().expect("run");
        assert!(!output.status.success(), "outside write should fail");
        assert!(inside_file.exists(), "inside write should succeed");
        assert!(!outside_file.exists(), "outside write should be blocked");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_deny_default_blocks_reads_outside_sandbox() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("tempdir");
        let sandbox_dir = temp.path().join("sandbox");
        let secret_dir = temp.path().join("secrets");
        std::fs::create_dir_all(&sandbox_dir).expect("mkdir sandbox");
        std::fs::create_dir_all(&secret_dir).expect("mkdir secrets");

        // Write a file outside the sandbox that we'll try to read
        let secret_file = secret_dir.join("secret.txt");
        std::fs::write(&secret_file, "top-secret").expect("write secret");

        // Write a file inside the readable area
        let readable_file = sandbox_dir.join("allowed.txt");
        std::fs::write(&readable_file, "ok-to-read").expect("write allowed");

        let cfg = SandboxConfig {
            writable_paths: vec![sandbox_dir.clone()],
            readable_paths: vec![],
        };
        let cfg = normalize_scope(&cfg, Path::new("/")).expect("normalize");

        // Try to read secret file — should fail due to deny-default
        let (program, argv) = sandboxed_spawn_command(
            "/bin/sh",
            &[
                "-lc".to_string(),
                format!("cat \"{}\"", secret_file.display()),
            ],
            Some(&cfg),
        )
        .expect("spawn wrapper");

        let output = Command::new(program).args(argv).output().expect("run");
        assert!(
            !output.status.success(),
            "reading outside sandbox should fail under deny-default"
        );

        // Read inside sandbox should succeed
        let (program2, argv2) = sandboxed_spawn_command(
            "/bin/sh",
            &[
                "-lc".to_string(),
                format!("cat \"{}\"", readable_file.display()),
            ],
            Some(&cfg),
        )
        .expect("spawn wrapper");

        let output2 = Command::new(program2).args(argv2).output().expect("run");
        assert!(
            output2.status.success(),
            "reading inside sandbox should succeed"
        );
        let stdout = String::from_utf8_lossy(&output2.stdout);
        assert!(stdout.contains("ok-to-read"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_wrapper_uses_unshare() {
        let cfg = SandboxConfig {
            writable_paths: vec![PathBuf::from("/tmp")],
        };
        let (exec, args) =
            sandboxed_spawn_command("/bin/echo", &["ok".to_string()], Some(&cfg)).expect("wrap");
        assert!(exec.ends_with("tmax-sandbox-runner"));
        assert!(args.iter().any(|a| a == "--writable"));
        assert!(args.iter().any(|a| a == "--exec"));
        assert!(args.iter().any(|a| a == "/bin/echo"));
    }
}
