//! Service management: install/uninstall/status for launchd (macOS) and systemd (Linux).
//!
//! With 1Password configured, the node daemon authenticates automatically on start.
//! Without 1Password, the service will fail to start (interactive TOTP required).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "macos")]
const LABEL: &str = "ai.ardabot.agentbook-node";

/// Install and start the node daemon as a system service.
pub fn cmd_service_install(
    state_dir: Option<PathBuf>,
    relay_hosts: Vec<String>,
    no_relay: bool,
    rpc_url: Option<String>,
    yolo: bool,
) -> Result<()> {
    let node_bin = find_node_binary()?;
    let state_dir = resolve_state_dir(state_dir)?;
    let socket_path = agentbook::client::default_socket_path();

    let log_dir = state_dir.join("logs");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("failed to create log dir {}", log_dir.display()))?;

    // Warn if 1Password is not set up — the service cannot authenticate without it.
    let op_title = agentbook_wallet::onepassword::item_title_from_state_dir(&state_dir);
    let has_op = agentbook_wallet::onepassword::has_op_cli()
        && op_title
            .as_ref()
            .map(|t| agentbook_wallet::onepassword::has_agentbook_item(t))
            .unwrap_or(false);

    if !yolo && !has_op {
        eprintln!();
        eprintln!("  \x1b[1;33mWarning: 1Password CLI not detected.\x1b[0m");
        eprintln!("  The service needs 1Password to authenticate non-interactively at boot.");
        eprintln!("  Without it, the service will fail to start.");
        eprintln!("  Re-run `agentbook-cli setup` with the `op` CLI installed to enable auto-start,");
        eprintln!("  or use `agentbook-cli up` for interactive startup instead.");
        eprintln!();
    }

    install_platform(
        &node_bin,
        &socket_path,
        &state_dir,
        &log_dir,
        relay_hosts,
        no_relay,
        rpc_url,
        yolo,
    )
}

/// Stop and remove the node daemon service.
pub fn cmd_service_uninstall() -> Result<()> {
    uninstall_platform()
}

/// Show the current service status.
pub fn cmd_service_status() -> Result<()> {
    status_platform()
}

// ── macOS (launchd) ──────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env var not set")?;
    let dir = PathBuf::from(home).join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn install_platform(
    node_bin: &Path,
    socket_path: &Path,
    state_dir: &Path,
    log_dir: &Path,
    relay_hosts: Vec<String>,
    no_relay: bool,
    rpc_url: Option<String>,
    yolo: bool,
) -> Result<()> {
    let home = std::env::var("HOME").context("HOME env var not set")?;

    // Build ProgramArguments entries
    let mut args_xml = format!(
        "        <string>{}</string>\n",
        node_bin.display()
    );
    args_xml += &format!(
        "        <string>--socket</string>\n        <string>{}</string>\n",
        socket_path.display()
    );
    args_xml += &format!(
        "        <string>--state-dir</string>\n        <string>{}</string>\n",
        state_dir.display()
    );
    if no_relay {
        args_xml += "        <string>--no-relay</string>\n";
    } else {
        for host in &relay_hosts {
            args_xml += &format!(
                "        <string>--relay-host</string>\n        <string>{host}</string>\n"
            );
        }
    }
    if let Some(ref url) = rpc_url {
        args_xml += &format!(
            "        <string>--rpc-url</string>\n        <string>{url}</string>\n"
        );
    }
    if yolo {
        args_xml += "        <string>--yolo</string>\n";
    }

    let log_out = log_dir.join("node.log");
    let log_err = log_dir.join("node-error.log");

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args_xml}    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>HOME</key>
        <string>{home}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_out}</string>
    <key>StandardErrorPath</key>
    <string>{log_err}</string>
</dict>
</plist>
"#,
        log_out = log_out.display(),
        log_err = log_err.display(),
    );

    let plist_path = plist_path()?;

    // Bootout any existing instance before writing the new plist.
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist_path.to_string_lossy()])
        .output();

    std::fs::write(&plist_path, plist)
        .with_context(|| format!("failed to write plist to {}", plist_path.display()))?;

    let status = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_path.to_string_lossy()])
        .status()
        .context("failed to run launchctl bootstrap")?;

    if !status.success() {
        bail!("launchctl bootstrap failed — check {} for errors", log_err.display());
    }

    println!("Service installed and started.");
    println!("  Plist  : {}", plist_path.display());
    println!("  Logs   : {}", log_dir.display());
    println!("  Status : agentbook-cli service status");
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_platform() -> Result<()> {
    let plist_path = plist_path()?;
    if !plist_path.exists() {
        println!("Service is not installed.");
        return Ok(());
    }
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist_path.to_string_lossy()])
        .status();
    std::fs::remove_file(&plist_path)
        .with_context(|| format!("failed to remove {}", plist_path.display()))?;
    println!("Service uninstalled.");
    Ok(())
}

#[cfg(target_os = "macos")]
fn status_platform() -> Result<()> {
    let output = Command::new("launchctl")
        .args(["list", LABEL])
        .output()
        .context("failed to run launchctl")?;
    if output.status.success() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        println!("Service not loaded (not running).");
        println!("  Install : agentbook-cli service install");
    }
    Ok(())
}

// ── Linux (systemd user session) ─────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn service_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env var not set")?;
    let dir = PathBuf::from(home).join(".config/systemd/user");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("agentbook-node.service"))
}

#[cfg(target_os = "linux")]
fn install_platform(
    node_bin: &Path,
    socket_path: &Path,
    state_dir: &Path,
    log_dir: &Path,
    relay_hosts: Vec<String>,
    no_relay: bool,
    rpc_url: Option<String>,
    yolo: bool,
) -> Result<()> {
    let mut exec = node_bin.display().to_string();
    exec += &format!(
        " --socket {} --state-dir {}",
        socket_path.display(),
        state_dir.display()
    );
    if no_relay {
        exec += " --no-relay";
    } else {
        for host in &relay_hosts {
            exec += &format!(" --relay-host {host}");
        }
    }
    if let Some(ref url) = rpc_url {
        exec += &format!(" --rpc-url {url}");
    }
    if yolo {
        exec += " --yolo";
    }

    let log_out = log_dir.join("node.log");
    let log_err = log_dir.join("node-error.log");

    let unit = format!(
        r#"[Unit]
Description=agentbook node daemon
After=network.target

[Service]
ExecStart={exec}
Restart=on-failure
RestartSec=5
StandardOutput=append:{log_out}
StandardError=append:{log_err}

[Install]
WantedBy=default.target
"#,
        log_out = log_out.display(),
        log_err = log_err.display(),
    );

    let service_path = service_path()?;
    std::fs::write(&service_path, unit)
        .with_context(|| format!("failed to write service file to {}", service_path.display()))?;

    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", "agentbook-node.service"])?;
    run_systemctl(&["--user", "start", "agentbook-node.service"])?;

    println!("Service installed and started.");
    println!("  Service : {}", service_path.display());
    println!("  Logs    : {}", log_dir.display());
    println!("  Status  : agentbook-cli service status");
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_platform() -> Result<()> {
    let service_path = service_path()?;
    if !service_path.exists() {
        println!("Service is not installed.");
        return Ok(());
    }
    let _ = run_systemctl(&["--user", "stop", "agentbook-node.service"]);
    let _ = run_systemctl(&["--user", "disable", "agentbook-node.service"]);
    std::fs::remove_file(&service_path)
        .with_context(|| format!("failed to remove {}", service_path.display()))?;
    let _ = run_systemctl(&["--user", "daemon-reload"]);
    println!("Service uninstalled.");
    Ok(())
}

#[cfg(target_os = "linux")]
fn status_platform() -> Result<()> {
    let output = Command::new("systemctl")
        .args(["--user", "status", "agentbook-node.service", "--no-pager"])
        .output()
        .context("failed to run systemctl")?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .context("failed to run systemctl")?;
    if !status.success() {
        bail!("systemctl {} failed (exit {status})", args.join(" "));
    }
    Ok(())
}

// ── Unsupported platforms ─────────────────────────────────────────────────────

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install_platform(
    _node_bin: &Path,
    _socket_path: &Path,
    _state_dir: &Path,
    _log_dir: &Path,
    _relay_hosts: Vec<String>,
    _no_relay: bool,
    _rpc_url: Option<String>,
    _yolo: bool,
) -> Result<()> {
    bail!("service management is only supported on macOS and Linux")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn uninstall_platform() -> Result<()> {
    bail!("service management is only supported on macOS and Linux")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn status_platform() -> Result<()> {
    bail!("service management is only supported on macOS and Linux")
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn find_node_binary() -> Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("agentbook-node");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Fallback: expect it on PATH
    Ok(PathBuf::from("agentbook-node"))
}

fn resolve_state_dir(state_dir: Option<PathBuf>) -> Result<PathBuf> {
    match state_dir {
        Some(d) => Ok(d),
        None => agentbook_mesh::state_dir::default_state_dir(),
    }
}
