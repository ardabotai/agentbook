//! Integration tests for sandbox enforcement.
//!
//! These tests spawn real PTY processes with sandbox-exec on macOS and verify
//! that filesystem write restrictions are enforced correctly.

use std::path::PathBuf;
use std::time::Duration;

use libtmax::session::{SessionCreateConfig, SessionManager};
use tmax_protocol::{SandboxConfig, SessionId};

/// Helper: create a session config that runs a shell command.
///
/// Defaults to no sandbox and no parent. Use `sandbox` and `parent_id` to
/// override when testing nesting behaviour.
fn sandboxed_session_config(
    cmd: &str,
    sandbox: Option<SandboxConfig>,
    parent_id: Option<SessionId>,
) -> SessionCreateConfig {
    SessionCreateConfig {
        exec: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), cmd.to_string()],
        cwd: None,
        label: Some("sandbox-test".to_string()),
        sandbox,
        parent_id,
        cols: 80,
        rows: 24,
    }
}

/// Convenience: build a `SandboxConfig` from writable paths.
fn sandbox_with(writable_paths: Vec<PathBuf>) -> Option<SandboxConfig> {
    Some(SandboxConfig { writable_paths })
}

/// Wait for the session's child process to exit, returning `true` if it
/// exited within `timeout` or `false` on timeout.
fn wait_for_exit(mgr: &mut SessionManager, session_id: &SessionId, timeout: Duration) -> bool {
    let mut child = mgr.take_child(session_id).unwrap();
    let handle = std::thread::spawn(move || {
        let _ = child.wait();
    });
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if handle.is_finished() {
            let _ = handle.join();
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_allows_write_inside_scope() {
    let dir = tempfile::tempdir().unwrap();
    let test_file = dir.path().join("allowed.txt");
    let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();

    let cmd = format!("echo 'hello sandbox' > '{}'", test_file.display());
    let config = sandboxed_session_config(&cmd, sandbox_with(vec![canonical_dir]), None);

    let mut mgr = SessionManager::new();
    let (session_id, _rx) = mgr.create_session(config).unwrap();

    wait_for_exit(&mut mgr, &session_id, Duration::from_secs(5));

    assert!(test_file.exists(), "File should have been created inside sandbox scope");
    let content = std::fs::read_to_string(&test_file).unwrap();
    assert!(
        content.contains("hello sandbox"),
        "File content should be 'hello sandbox', got: {content}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_write_outside_scope() {
    let allowed_dir = tempfile::tempdir().unwrap();
    let denied_dir = tempfile::tempdir().unwrap();
    let canonical_allowed = std::fs::canonicalize(allowed_dir.path()).unwrap();

    let denied_file = denied_dir.path().join("denied.txt");
    let cmd = format!("echo 'should fail' > '{}'", denied_file.display());
    let config = sandboxed_session_config(&cmd, sandbox_with(vec![canonical_allowed]), None);

    let mut mgr = SessionManager::new();
    let (session_id, _rx) = mgr.create_session(config).unwrap();

    wait_for_exit(&mut mgr, &session_id, Duration::from_secs(5));

    assert!(
        !denied_file.exists(),
        "File should NOT have been created outside sandbox scope"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_nesting_child_inherits_parent() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();

    let parent_config = sandboxed_session_config(
        "sleep 10",
        sandbox_with(vec![canonical.clone()]),
        None,
    );

    let mut mgr = SessionManager::new();
    let (parent_id, _rx) = mgr.create_session(parent_config).unwrap();

    // Child without explicit sandbox — should inherit parent's
    let child_config = sandboxed_session_config(
        "echo inherited",
        None,
        Some(parent_id),
    );

    let (child_id, _rx) = mgr.create_session(child_config).unwrap();

    let child_info = mgr.get_session_info(&child_id).unwrap();
    assert!(
        child_info.sandbox.is_some(),
        "Child should have inherited parent's sandbox"
    );
    assert_eq!(
        child_info.sandbox.unwrap().writable_paths,
        vec![canonical],
        "Inherited sandbox should have parent's writable paths"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_nesting_violation_rejected() {
    let parent_dir = tempfile::tempdir().unwrap();
    let child_dir = tempfile::tempdir().unwrap();
    let canonical_parent = std::fs::canonicalize(parent_dir.path()).unwrap();
    let canonical_child = std::fs::canonicalize(child_dir.path()).unwrap();

    let mut mgr = SessionManager::new();

    let parent_config = sandboxed_session_config(
        "sleep 10",
        sandbox_with(vec![canonical_parent]),
        None,
    );
    let (parent_id, _rx) = mgr.create_session(parent_config).unwrap();

    // Child with sandbox outside parent's scope — should fail
    let child_config = sandboxed_session_config(
        "echo escape",
        sandbox_with(vec![canonical_child]),
        Some(parent_id),
    );

    let result = mgr.create_session(child_config);
    assert!(
        result.is_err(),
        "Child with paths outside parent's scope should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("nesting violation") || err.contains("not within"),
        "Error should mention nesting violation, got: {err}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_nesting_child_subset_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("subdir");
    std::fs::create_dir(&subdir).unwrap();
    let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
    let canonical_subdir = std::fs::canonicalize(&subdir).unwrap();

    let mut mgr = SessionManager::new();

    let parent_config = sandboxed_session_config(
        "sleep 10",
        sandbox_with(vec![canonical_dir]),
        None,
    );
    let (parent_id, _rx) = mgr.create_session(parent_config).unwrap();

    // Child with sandbox scoped to subdir (subset of parent) — should succeed
    let child_config = sandboxed_session_config(
        "echo ok",
        sandbox_with(vec![canonical_subdir]),
        Some(parent_id),
    );

    let result = mgr.create_session(child_config);
    assert!(
        result.is_ok(),
        "Child with paths inside parent's scope should be allowed"
    );
}
