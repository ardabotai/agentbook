//! Integration tests for sandbox enforcement.
//!
//! These tests spawn real PTY processes with sandbox-exec on macOS and verify
//! that filesystem write restrictions are enforced correctly.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use libtmax::session::{SessionCreateConfig, SessionManager};
use tmax_protocol::SandboxConfig;

/// Helper: create a sandboxed session that runs a shell command.
fn sandboxed_session_config(
    cmd: &str,
    writable_paths: Vec<PathBuf>,
    parent_id: Option<String>,
) -> SessionCreateConfig {
    SessionCreateConfig {
        exec: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), cmd.to_string()],
        cwd: None,
        label: Some("sandbox-test".to_string()),
        sandbox: Some(SandboxConfig {
            writable_paths,
            inherit_parent: true,
        }),
        parent_id,
        cols: 80,
        rows: 24,
    }
}

/// Wait for the child process to exit by polling.
fn wait_for_exit(mgr: &mut SessionManager, session_id: &String, timeout: Duration) {
    let child = mgr.take_child(session_id).unwrap();
    let handle = thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if handle.is_finished() {
            let _ = handle.join();
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    // Timeout — process may still be running
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_allows_write_inside_scope() {
    let dir = tempfile::tempdir().unwrap();
    let test_file = dir.path().join("allowed.txt");
    let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();

    let cmd = format!("echo 'hello sandbox' > '{}'", test_file.display());
    let config = sandboxed_session_config(&cmd, vec![canonical_dir], None);

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
    let config = sandboxed_session_config(&cmd, vec![canonical_allowed], None);

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

    // Create parent session with sandbox
    let parent_config = SessionCreateConfig {
        exec: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "sleep 10".to_string()],
        cwd: None,
        label: Some("parent".to_string()),
        sandbox: Some(SandboxConfig {
            writable_paths: vec![canonical.clone()],
            inherit_parent: true,
        }),
        parent_id: None,
        cols: 80,
        rows: 24,
    };

    let mut mgr = SessionManager::new();
    let (parent_id, _rx) = mgr.create_session(parent_config).unwrap();

    // Create child without explicit sandbox — should inherit parent's
    let child_config = SessionCreateConfig {
        exec: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "echo inherited".to_string()],
        cwd: None,
        label: Some("child".to_string()),
        sandbox: None, // No explicit sandbox — should inherit
        parent_id: Some(parent_id.clone()),
        cols: 80,
        rows: 24,
    };

    let (child_id, _rx) = mgr.create_session(child_config).unwrap();

    // Verify child session was created with inherited sandbox
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

    // Create parent with sandbox scoped to parent_dir
    let parent_config = SessionCreateConfig {
        exec: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "sleep 10".to_string()],
        cwd: None,
        label: Some("parent".to_string()),
        sandbox: Some(SandboxConfig {
            writable_paths: vec![canonical_parent],
            inherit_parent: true,
        }),
        parent_id: None,
        cols: 80,
        rows: 24,
    };

    let (parent_id, _rx) = mgr.create_session(parent_config).unwrap();

    // Try to create child with sandbox outside parent's scope — should fail
    let child_config = SessionCreateConfig {
        exec: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "echo escape".to_string()],
        cwd: None,
        label: Some("child".to_string()),
        sandbox: Some(SandboxConfig {
            writable_paths: vec![canonical_child],
            inherit_parent: true,
        }),
        parent_id: Some(parent_id),
        cols: 80,
        rows: 24,
    };

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

    // Create parent with sandbox scoped to dir
    let parent_config = SessionCreateConfig {
        exec: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "sleep 10".to_string()],
        cwd: None,
        label: Some("parent".to_string()),
        sandbox: Some(SandboxConfig {
            writable_paths: vec![canonical_dir],
            inherit_parent: true,
        }),
        parent_id: None,
        cols: 80,
        rows: 24,
    };

    let (parent_id, _rx) = mgr.create_session(parent_config).unwrap();

    // Create child with sandbox scoped to subdir (subset of parent) — should succeed
    let child_config = SessionCreateConfig {
        exec: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "echo ok".to_string()],
        cwd: None,
        label: Some("child".to_string()),
        sandbox: Some(SandboxConfig {
            writable_paths: vec![canonical_subdir],
            inherit_parent: true,
        }),
        parent_id: Some(parent_id),
        cols: 80,
        rows: 24,
    };

    let result = mgr.create_session(child_config);
    assert!(
        result.is_ok(),
        "Child with paths inside parent's scope should be allowed"
    );
}
