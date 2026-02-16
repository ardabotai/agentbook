#![cfg(target_os = "linux")]

use anyhow::{Result, bail};
use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn is_user_namespace_unavailable(stderr: &str) -> bool {
    stderr.contains("failed to unshare user/mount namespaces")
        || stderr.contains("Operation not permitted")
}

fn run_runner(script: &str, writable_dir: &std::path::Path) -> Result<std::process::Output> {
    let bin = assert_cmd::cargo::cargo_bin!("tmax-sandbox-runner");
    let output = Command::new(bin)
        .arg("--writable")
        .arg(writable_dir)
        .arg("--exec")
        .arg("/bin/sh")
        .arg("--")
        .arg("-lc")
        .arg(script)
        .output()?;
    Ok(output)
}

#[test]
fn allows_writes_inside_declared_scope() -> Result<()> {
    let temp = tempdir()?;
    let inside = temp.path().join("inside");
    fs::create_dir_all(&inside)?;
    let inside_file = inside.join("ok.txt");

    let script = format!("echo ok > \"{}\"", inside_file.display());
    let output = run_runner(&script, &inside)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && is_user_namespace_unavailable(&stderr) {
        eprintln!("skipping linux isolation assert: user namespaces unavailable ({stderr})");
        return Ok(());
    }
    if !output.status.success() {
        bail!(
            "sandbox runner failed unexpectedly: {}\nstderr:\n{}",
            output.status,
            stderr
        );
    }

    assert!(inside_file.exists(), "inside file should be created");
    Ok(())
}

#[test]
fn blocks_writes_outside_declared_scope() -> Result<()> {
    let temp = tempdir()?;
    let inside = temp.path().join("inside");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&inside)?;
    fs::create_dir_all(&outside)?;
    let inside_file = inside.join("ok.txt");
    let outside_file = outside.join("blocked.txt");

    let script = format!(
        "echo ok > \"{}\"; echo blocked > \"{}\"",
        inside_file.display(),
        outside_file.display()
    );
    let output = run_runner(&script, &inside)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && is_user_namespace_unavailable(&stderr) {
        eprintln!("skipping linux isolation assert: user namespaces unavailable ({stderr})");
        return Ok(());
    }

    assert!(
        !output.status.success(),
        "outside write should fail; status={} stderr={}",
        output.status,
        stderr
    );
    assert!(inside_file.exists(), "inside write should succeed");
    assert!(!outside_file.exists(), "outside write should be blocked");
    Ok(())
}
