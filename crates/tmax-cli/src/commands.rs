use std::io::Write;

use crossterm::terminal;
use tmax_protocol::{AttachMode, Request, Response, SandboxConfig, SessionId};

use crate::client::TmaxClient;

/// Extract response data or exit with error message.
fn unwrap_response(resp: Response) -> Option<serde_json::Value> {
    match resp {
        Response::Ok { data } => data,
        Response::Error { message, .. } => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        _ => None,
    }
}

/// Start the server daemon.
pub async fn server_start(foreground: bool) -> anyhow::Result<()> {
    if foreground {
        let status = tokio::process::Command::new("tmax-server")
            .status()
            .await?;
        std::process::exit(status.code().unwrap_or(1));
    } else {
        let child = std::process::Command::new("tmax-server")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .spawn()?;

        println!("tmax server started (pid: {})", child.id());
        Ok(())
    }
}

/// Stop the server daemon.
pub async fn server_stop() -> anyhow::Result<()> {
    let pid_path = tmax_protocol::paths::pid_file_path();
    if pid_path.exists() {
        let pid_str = std::fs::read_to_string(&pid_path)?;
        let pid: i32 = pid_str.trim().parse()?;
        if pid <= 1 {
            anyhow::bail!("invalid PID in pid file: {pid}");
        }
        // SAFETY: pid is validated as > 1, and SIGTERM is a standard signal
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let _ = std::fs::remove_file(&pid_path);
        println!("tmax server stopped (pid: {pid})");
    } else {
        println!("tmax server is not running");
    }
    Ok(())
}

/// Check server status.
pub async fn server_status() -> anyhow::Result<()> {
    let pid_path = tmax_protocol::paths::pid_file_path();
    if pid_path.exists() {
        let pid_str = std::fs::read_to_string(&pid_path)?;
        let pid: i32 = pid_str.trim().parse()?;
        if pid <= 1 {
            anyhow::bail!("invalid PID in pid file: {pid}");
        }
        // SAFETY: pid is validated as > 1, and signal 0 only checks process existence
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if alive {
            println!("tmax server is running (pid: {pid})");
        } else {
            println!("tmax server is not running (stale pid file)");
            let _ = std::fs::remove_file(&pid_path);
        }
    } else {
        println!("tmax server is not running");
    }
    Ok(())
}

/// Create a new session.
pub async fn session_new(
    exec: String,
    label: Option<String>,
    sandbox: Option<SandboxConfig>,
    parent: Option<SessionId>,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;

    let parts: Vec<&str> = exec.split_whitespace().collect();
    let (cmd, args) = if parts.len() > 1 {
        (
            parts[0].to_string(),
            parts[1..].iter().map(|s| s.to_string()).collect(),
        )
    } else {
        (exec, vec![])
    };

    let req = Request::SessionCreate {
        exec: cmd,
        args,
        cwd: None,
        label: label.clone(),
        sandbox,
        parent_id: parent,
        cols,
        rows,
    };

    if let Some(data) = unwrap_response(client.request(&req).await?) {
        let session_id = data["session_id"].as_str().unwrap_or("unknown");
        println!("{session_id}");
    }
    Ok(())
}

/// List sessions.
pub async fn session_list(tree: bool) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;

    let req = if tree {
        Request::SessionTree
    } else {
        Request::SessionList
    };

    if let Some(data) = unwrap_response(client.request(&req).await?) {
        if tree {
            let nodes: Vec<tmax_protocol::SessionTreeNode> =
                serde_json::from_value(data)?;
            for node in &nodes {
                print_tree_node(node, 0);
            }
            if nodes.is_empty() {
                println!("no sessions");
            }
        } else {
            let sessions: Vec<tmax_protocol::SessionInfo> =
                serde_json::from_value(data)?;
            if sessions.is_empty() {
                println!("no sessions");
            } else {
                println!(
                    "{:<36}  {:<15}  {:<20}  {:<6}  ATTACHMENTS",
                    "ID", "LABEL", "EXEC", "STATUS"
                );
                for s in &sessions {
                    let status = if s.exited { "exited" } else { "running" };
                    let label = s.label.as_deref().unwrap_or("-");
                    println!(
                        "{:<36}  {:<15}  {:<20}  {:<6}  {} ({}E)",
                        s.id, label, s.exec, status, s.attachment_count,
                        s.edit_attachment_count
                    );
                }
            }
        }
    }
    Ok(())
}

fn print_tree_node(node: &tmax_protocol::SessionTreeNode, depth: usize) {
    let indent = "  ".repeat(depth);
    let prefix = if depth > 0 { "|- " } else { "" };
    let label = node.info.label.as_deref().unwrap_or("-");
    let status = if node.info.exited { "exited" } else { "running" };
    println!(
        "{indent}{prefix}[{id}] {label} ({exec}) [{status}]",
        id = &node.info.id[..8],
        exec = node.info.exec,
    );
    for child in &node.children {
        print_tree_node(child, depth + 1);
    }
}

/// Get session info.
pub async fn session_info(session_id: String) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;
    let req = Request::SessionInfo { session_id };

    if let Some(data) = unwrap_response(client.request(&req).await?) {
        let info: tmax_protocol::SessionInfo = serde_json::from_value(data)?;
        println!("ID:          {}", info.id);
        println!("Label:       {}", info.label.as_deref().unwrap_or("-"));
        println!("Exec:        {} {}", info.exec, info.args.join(" "));
        println!("CWD:         {}", info.cwd.display());
        println!("Parent:      {}", info.parent_id.as_deref().unwrap_or("-"));
        println!("Children:    {}", info.children.len());
        println!(
            "Status:      {}",
            if info.exited { "exited" } else { "running" }
        );
        if let Some(code) = info.exit_code {
            println!("Exit code:   {code}");
        }
        if let Some(ref sb) = info.sandbox {
            let paths: Vec<_> = sb.writable_paths.iter().map(|p| p.display().to_string()).collect();
            println!("Sandbox:     {}", paths.join(", "));
        } else {
            println!("Sandbox:     none");
        }
        println!(
            "Attachments: {} ({} edit)",
            info.attachment_count, info.edit_attachment_count
        );
    }
    Ok(())
}

/// Attach to a session with terminal forwarding.
pub async fn session_attach(session_id: String, view: bool) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;

    let mode = if view {
        AttachMode::View
    } else {
        AttachMode::Edit
    };

    let req = Request::Attach {
        session_id: session_id.clone(),
        mode,
    };
    unwrap_response(client.request(&req).await?);

    let req = Request::Subscribe {
        session_id: session_id.clone(),
        last_seq: None,
    };
    unwrap_response(client.request(&req).await?);

    terminal::enable_raw_mode()?;
    let _raw_guard = RawModeGuard;

    println!("\r\n[attached to {session_id} in {mode:?} mode - Ctrl+\\ to detach]\r");

    loop {
        tokio::select! {
            line = client.read_line() => {
                match line {
                    Ok(Some(Response::Event(tmax_protocol::Event::Output { data, .. }))) => {
                        let mut stdout = std::io::stdout();
                        stdout.write_all(&data)?;
                        stdout.flush()?;
                    }
                    Ok(Some(Response::Event(tmax_protocol::Event::SessionExited { exit_code, .. }))) => {
                        println!("\r\n[session exited with code {exit_code:?}]\r");
                        break;
                    }
                    Ok(None) => {
                        println!("\r\n[server disconnected]\r");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// Send input to a session.
pub async fn session_send(session_id: String, input: String) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;
    let req = Request::SendInput {
        session_id,
        data: input.into_bytes(),
    };
    unwrap_response(client.request(&req).await?);
    Ok(())
}

/// Resize a session.
pub async fn session_resize(session_id: String, cols: u16, rows: u16) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;
    let req = Request::Resize {
        session_id,
        cols,
        rows,
    };
    unwrap_response(client.request(&req).await?);
    println!("resized to {cols}x{rows}");
    Ok(())
}

/// Kill a session.
pub async fn session_kill(session_id: String, cascade: bool) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;
    let req = Request::SessionDestroy {
        session_id,
        cascade,
    };
    if let Some(data) = unwrap_response(client.request(&req).await?) {
        if let Some(destroyed) = data.get("destroyed") {
            println!("destroyed: {destroyed}");
        }
    }
    Ok(())
}

/// Insert a marker.
pub async fn session_marker(session_id: String, name: String) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;
    let req = Request::MarkerInsert { session_id, name };
    if let Some(data) = unwrap_response(client.request(&req).await?) {
        if let Some(seq) = data.get("seq") {
            println!("marker inserted at seq {seq}");
        }
    }
    Ok(())
}

/// List markers.
pub async fn session_markers(session_id: String) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;
    let req = Request::MarkerList { session_id };
    if let Some(data) = unwrap_response(client.request(&req).await?) {
        let markers: Vec<tmax_protocol::MarkerInfo> = serde_json::from_value(data)?;
        if markers.is_empty() {
            println!("no markers");
        } else {
            println!("{:<20}  {:<10}", "NAME", "SEQ");
            for m in &markers {
                println!("{:<20}  {:<10}", m.name, m.seq);
            }
        }
    }
    Ok(())
}

/// Stream raw output to stdout.
pub async fn session_stream(session_id: String) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;

    let req = Request::Subscribe {
        session_id: session_id.clone(),
        last_seq: None,
    };
    unwrap_response(client.request(&req).await?);

    loop {
        match client.read_line().await? {
            Some(Response::Event(tmax_protocol::Event::Output { data, .. })) => {
                let mut stdout = std::io::stdout();
                stdout.write_all(&data)?;
                stdout.flush()?;
            }
            Some(Response::Event(tmax_protocol::Event::SessionExited { .. })) => {
                break;
            }
            None => break,
            _ => {}
        }
    }
    Ok(())
}

/// Stream JSON events to stdout.
pub async fn session_subscribe(session_id: String, since: Option<u64>) -> anyhow::Result<()> {
    let mut client = TmaxClient::connect().await?;

    let req = Request::Subscribe {
        session_id: session_id.clone(),
        last_seq: since,
    };
    unwrap_response(client.request(&req).await?);

    while let Some(resp) = client.read_line().await? {
        let json = serde_json::to_string(&resp)?;
        println!("{json}");
    }
    Ok(())
}

