use anyhow::{Context, Result, anyhow, bail};
use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tmax_protocol::{AttachMode, Request, Response, SessionSummary};

fn spawn_mock_server(socket_path: PathBuf) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }

        let listener = UnixListener::bind(&socket_path).context("failed to bind mock socket")?;
        let (stream, _) = listener.accept().context("failed to accept client")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("failed to set read timeout")?;

        let read_half = stream.try_clone().context("failed to clone stream")?;
        let mut reader = BufReader::new(read_half);
        let mut writer = BufWriter::new(stream);

        writeln!(
            writer,
            "{}",
            serde_json::to_string(&Response::hello(vec![]))?
        )?;
        writer.flush()?;

        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("client disconnected before SessionInfo");
        }
        match serde_json::from_str::<Request>(line.trim_end())? {
            Request::SessionInfo { session_id } if session_id == "s-integration" => {}
            other => bail!("expected SessionInfo(s-integration), got {other:?}"),
        }

        let summary = SessionSummary {
            session_id: "s-integration".to_string(),
            label: Some("integration".to_string()),
            tags: Vec::new(),
            exec: "/bin/cat".to_string(),
            cwd: std::env::current_dir().context("current_dir")?,
            parent_id: None,
            created_at_ms: 1,
            sandboxed: false,
            git_branch: None,
            git_repo_root: None,
            git_worktree_path: None,
            git_dirty: None,
        };
        writeln!(
            writer,
            "{}",
            serde_json::to_string(&Response::ok(Some(serde_json::to_value(summary)?)))?
        )?;
        writer.flush()?;

        line.clear();
        if reader.read_line(&mut line)? == 0 {
            bail!("client disconnected before Attach");
        }
        match serde_json::from_str::<Request>(line.trim_end())? {
            Request::Attach {
                session_id,
                mode,
                last_seq_seen,
            } if session_id == "s-integration"
                && mode == AttachMode::View
                && last_seq_seen.is_none() => {}
            other => bail!("expected view Attach, got {other:?}"),
        }

        let attach_ok = serde_json::json!({
            "attachment": {
                "attachment_id": "a-1"
            }
        });
        writeln!(
            writer,
            "{}",
            serde_json::to_string(&Response::ok(Some(attach_ok)))?
        )?;
        writer.flush()?;

        line.clear();
        if reader.read_line(&mut line)? == 0 {
            bail!("client disconnected before Detach");
        }
        match serde_json::from_str::<Request>(line.trim_end())? {
            Request::Detach { attachment_id } if attachment_id == "a-1" => {}
            other => bail!("expected Detach(a-1), got {other:?}"),
        }

        Ok(())
    })
}

fn client_bin() -> Command {
    cargo_bin_cmd!("tmax-client")
}

fn socket_for(temp: &Path) -> PathBuf {
    temp.join("tmax.sock")
}

#[test]
fn client_headless_smoke_completes_attach_and_detach() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());
    let server = spawn_mock_server(socket_path.clone());

    let output = client_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("--view")
        .arg("--headless-smoke")
        .arg("s-integration")
        .output()
        .context("failed to run tmax-client headless smoke")?;

    if !output.status.success() {
        return Err(anyhow!(
            "client failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    server.join().expect("mock server thread panicked")?;
    Ok(())
}
