use anyhow::{Context, Result, bail};
use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tmax_protocol::{Request, Response, SessionSummary, SharedTaskStatus};

fn spawn_mock_server<F>(socket_path: PathBuf, handler: F) -> thread::JoinHandle<Result<()>>
where
    F: FnOnce(Request) -> Result<Response> + Send + 'static,
{
    thread::spawn(move || {
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }

        let listener =
            UnixListener::bind(&socket_path).with_context(|| "failed to bind mock socket")?;
        let (stream, _) = listener.accept().context("failed to accept client")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("failed to set read timeout")?;

        let read_half = stream.try_clone().context("failed to clone stream")?;
        let mut reader = BufReader::new(read_half);
        let mut writer = BufWriter::new(stream);

        let hello = serde_json::to_string(&Response::hello(vec!["test".to_string()]))?;
        writeln!(writer, "{hello}")?;
        writer.flush()?;

        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            bail!("client disconnected before request");
        }

        let req: Request = serde_json::from_str(line.trim_end())?;
        let response = handler(req)?;
        writeln!(writer, "{}", serde_json::to_string(&response)?)?;
        writer.flush()?;

        Ok(())
    })
}

fn write_response(
    writer: &mut BufWriter<std::os::unix::net::UnixStream>,
    resp: &Response,
) -> Result<()> {
    writeln!(writer, "{}", serde_json::to_string(resp)?)?;
    writer.flush()?;
    Ok(())
}

fn read_request(reader: &mut BufReader<std::os::unix::net::UnixStream>) -> Result<Request> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        bail!("client disconnected before request");
    }
    let req: Request = serde_json::from_str(line.trim_end())?;
    Ok(req)
}

fn spawn_scripted_server<F>(socket_path: PathBuf, handler: F) -> thread::JoinHandle<Result<()>>
where
    F: FnOnce(
            &mut BufReader<std::os::unix::net::UnixStream>,
            &mut BufWriter<std::os::unix::net::UnixStream>,
        ) -> Result<()>
        + Send
        + 'static,
{
    thread::spawn(move || {
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }

        let listener =
            UnixListener::bind(&socket_path).with_context(|| "failed to bind mock socket")?;
        let (stream, _) = listener.accept().context("failed to accept client")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("failed to set read timeout")?;

        let read_half = stream.try_clone().context("failed to clone stream")?;
        let mut reader = BufReader::new(read_half);
        let mut writer = BufWriter::new(stream);

        write_response(&mut writer, &Response::hello(vec!["test".to_string()]))?;
        handler(&mut reader, &mut writer)
    })
}

fn cli_bin() -> Command {
    cargo_bin_cmd!("tmax")
}

fn socket_for(temp: &Path) -> PathBuf {
    temp.join("tmax.sock")
}

#[test]
fn cli_list_sends_session_list_request() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let server = spawn_mock_server(socket_path.clone(), move |req| match req {
        Request::SessionList => Ok(Response::ok(Some(json!([])))),
        other => bail!("expected SessionList, got {other:?}"),
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("list")
        .output()
        .context("failed to run tmax-cli list")?;

    assert!(
        output.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "[]");

    server.join().expect("mock server thread panicked")?;
    Ok(())
}

#[test]
fn cli_new_sends_session_create_request() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let expected_summary = SessionSummary {
        session_id: "s-123".to_string(),
        label: Some("demo".to_string()),
        tags: Vec::new(),
        exec: "/bin/echo".to_string(),
        cwd: std::env::current_dir().context("failed to read current dir")?,
        parent_id: None,
        created_at_ms: 1,
        sandboxed: false,
        git_branch: None,
        git_repo_root: None,
        git_worktree_path: None,
        git_dirty: None,
    };

    let expected_clone = expected_summary.clone();
    let server = spawn_mock_server(socket_path.clone(), move |req| match req {
        Request::SessionCreate {
            exec,
            args,
            tags,
            label,
            cols,
            rows,
            ..
        } => {
            if exec != "/bin/echo" {
                bail!("unexpected exec: {exec}");
            }
            if args != vec!["hello".to_string()] {
                bail!("unexpected args: {args:?}");
            }
            if !tags.is_empty() {
                bail!("unexpected tags: {tags:?}");
            }
            if label.as_deref() != Some("demo") {
                bail!("unexpected label: {label:?}");
            }
            if cols != 80 || rows != 24 {
                bail!("unexpected size: {cols}x{rows}");
            }
            Ok(Response::ok(Some(serde_json::to_value(&expected_clone)?)))
        }
        other => bail!("expected SessionCreate, got {other:?}"),
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("new")
        .arg("--exec")
        .arg("/bin/echo")
        .arg("--label")
        .arg("demo")
        .arg("--")
        .arg("hello")
        .output()
        .context("failed to run tmax-cli new")?;

    assert!(
        output.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"session_id\": \"s-123\""),
        "stdout was: {stdout}"
    );
    assert!(
        stdout.contains("\"exec\": \"/bin/echo\""),
        "stdout was: {stdout}"
    );

    server.join().expect("mock server thread panicked")?;
    Ok(())
}

#[test]
fn cli_new_defaults_to_shell_when_no_exec() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let server = spawn_mock_server(socket_path.clone(), move |req| match req {
        Request::SessionCreate { exec, .. } => {
            // Should default to $SHELL or /bin/sh
            assert!(!exec.is_empty(), "exec should default to a shell");
            Ok(Response::ok(Some(json!({ "session_id": "s-default", "exec": exec }))))
        }
        other => bail!("expected SessionCreate, got {other:?}"),
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("new")
        .output()
        .context("failed to run tmax-cli new")?;

    assert!(
        output.status.success(),
        "cli should succeed with default shell: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"session_id\": \"s-default\""),
        "stdout was: {stdout}"
    );

    server.join().expect("mock server thread panicked")?;
    Ok(())
}

#[test]
fn cli_new_wraps_bare_args_with_shell_c() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let server = spawn_mock_server(socket_path.clone(), move |req| match req {
        Request::SessionCreate { exec, args, .. } => {
            assert!(!exec.is_empty(), "exec should default to a shell");
            assert_eq!(args, vec!["-c".to_string(), "echo hello world".to_string()],
                "bare args should be wrapped with -c, got: {args:?}");
            Ok(Response::ok(Some(json!({ "session_id": "s-wrapped" }))))
        }
        other => bail!("expected SessionCreate, got {other:?}"),
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("new")
        .arg("echo")
        .arg("hello")
        .arg("world")
        .output()
        .context("failed to run tmax-cli new")?;

    assert!(
        output.status.success(),
        "cli should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    server.join().expect("mock server thread panicked")?;
    Ok(())
}

#[test]
fn cli_health_reports_healthy_json() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let server = spawn_mock_server(socket_path.clone(), move |req| match req {
        Request::SessionList => Ok(Response::ok(Some(json!([
            {"session_id": "s1"},
            {"session_id": "s2"}
        ])))),
        other => bail!("expected SessionList, got {other:?}"),
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("health")
        .arg("--json")
        .output()
        .context("failed to run tmax-cli health --json")?;

    assert!(
        output.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"healthy\": true"), "stdout was: {stdout}");
    assert!(
        stdout.contains("\"session_count\": 2"),
        "stdout was: {stdout}"
    );

    server.join().expect("mock server thread panicked")?;
    Ok(())
}

#[test]
fn cli_health_fails_when_socket_missing() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("health")
        .output()
        .context("failed to run tmax-cli health")?;

    assert!(!output.status.success(), "command should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("health check failed"),
        "stderr was: {stderr}"
    );
    Ok(())
}

#[test]
fn cli_run_task_streams_task_lifecycle_without_attachment_flags() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());
    let summary = SessionSummary {
        session_id: "s-task".to_string(),
        label: Some("task".to_string()),
        tags: vec!["agent".to_string()],
        exec: "/bin/echo".to_string(),
        cwd: std::env::current_dir().context("failed to read current dir")?,
        parent_id: None,
        created_at_ms: 1,
        sandboxed: false,
        git_branch: None,
        git_repo_root: None,
        git_worktree_path: None,
        git_dirty: None,
    };

    let summary_clone = summary.clone();
    let server = spawn_scripted_server(socket_path.clone(), move |reader, writer| {
        match read_request(reader)? {
            Request::SessionCreate { exec, args, .. } => {
                if exec != "/bin/echo" {
                    bail!("unexpected exec: {exec}");
                }
                if args != vec!["hello".to_string()] {
                    bail!("unexpected args: {args:?}");
                }
            }
            other => bail!("expected SessionCreate, got {other:?}"),
        }

        write_response(
            writer,
            &Response::ok(Some(serde_json::to_value(&summary_clone)?)),
        )?;

        match read_request(reader)? {
            Request::Subscribe {
                session_id,
                last_seq_seen,
            } => {
                if session_id != "s-task" {
                    bail!("unexpected session id: {session_id}");
                }
                if last_seq_seen != Some(0) {
                    bail!("unexpected last_seq_seen: {last_seq_seen:?}");
                }
            }
            other => bail!("expected Subscribe, got {other:?}"),
        }

        write_response(writer, &Response::ok(Some(json!({"subscribed": true}))))?;
        write_response(
            writer,
            &Response::Event {
                event: Box::new(tmax_protocol::Event::SessionExited {
                    session_id: "s-task".to_string(),
                    exit_code: Some(0),
                    signal: None,
                }),
            },
        )?;
        Ok(())
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("run-task")
        .arg("--exec")
        .arg("/bin/echo")
        .arg("--no-stream")
        .arg("--json")
        .arg("--")
        .arg("hello")
        .output()
        .context("failed to run tmax-cli run-task")?;

    assert!(
        output.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"session_id\": \"s-task\""),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("\"exit_code\": 0"), "stdout was: {stdout}");

    server.join().expect("mock server thread panicked")?;
    Ok(())
}

#[test]
fn cli_cancel_task_sends_destroy_without_manual_protocol_input() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let server = spawn_mock_server(socket_path.clone(), move |req| match req {
        Request::SessionDestroy {
            session_id,
            cascade,
        } => {
            if session_id != "s-cancel" {
                bail!("unexpected session id: {session_id}");
            }
            if !cascade {
                bail!("expected cascade=true");
            }
            Ok(Response::ok(Some(json!({"ok": true}))))
        }
        other => bail!("expected SessionDestroy, got {other:?}"),
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("cancel-task")
        .arg("s-cancel")
        .arg("--cascade")
        .arg("--json")
        .output()
        .context("failed to run tmax-cli cancel-task")?;

    assert!(
        output.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"session_id\": \"s-cancel\""),
        "stdout was: {stdout}"
    );
    assert!(
        stdout.contains("\"cancelled\": true"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("\"cascade\": true"), "stdout was: {stdout}");

    server.join().expect("mock server thread panicked")?;
    Ok(())
}

#[test]
fn cli_msg_send_sends_message_request() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let server = spawn_mock_server(socket_path.clone(), move |req| match req {
        Request::MessageSend {
            from_session_id,
            to_session_id,
            topic,
            body,
            requires_response,
            encrypt,
            sign,
        } => {
            if from_session_id.as_deref() != Some("root") {
                bail!("unexpected from_session_id: {from_session_id:?}");
            }
            if to_session_id != "child-1" {
                bail!("unexpected to_session_id: {to_session_id}");
            }
            if topic.as_deref() != Some("question") {
                bail!("unexpected topic: {topic:?}");
            }
            if body != "Need clarification" {
                bail!("unexpected body: {body}");
            }
            if !requires_response {
                bail!("expected requires_response=true");
            }
            if encrypt {
                bail!("expected encrypt=false");
            }
            if sign {
                bail!("expected sign=false");
            }
            Ok(Response::ok(Some(json!({"ok": true}))))
        }
        other => bail!("expected MessageSend, got {other:?}"),
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("msg")
        .arg("send")
        .arg("--from")
        .arg("root")
        .arg("--to")
        .arg("child-1")
        .arg("--topic")
        .arg("question")
        .arg("--requires-response")
        .arg("Need clarification")
        .output()
        .context("failed to run tmax-cli msg send")?;

    assert!(
        output.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    server.join().expect("mock server thread panicked")?;
    Ok(())
}

#[test]
fn cli_tasks_status_sends_set_status_request() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = socket_for(temp.path());

    let server = spawn_mock_server(socket_path.clone(), move |req| match req {
        Request::TaskSetStatus {
            task_id,
            session_id,
            status,
        } => {
            if task_id != "task-123" {
                bail!("unexpected task_id: {task_id}");
            }
            if session_id != "worker-1" {
                bail!("unexpected session_id: {session_id}");
            }
            if status != SharedTaskStatus::Done {
                bail!("unexpected status: {status:?}");
            }
            Ok(Response::ok(Some(
                json!({"task_id":"task-123","status":"done"}),
            )))
        }
        other => bail!("expected TaskSetStatus, got {other:?}"),
    });

    let output = cli_bin()
        .arg("--socket")
        .arg(&socket_path)
        .arg("tasks")
        .arg("status")
        .arg("task-123")
        .arg("worker-1")
        .arg("done")
        .output()
        .context("failed to run tmax-cli tasks status")?;

    assert!(
        output.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    server.join().expect("mock server thread panicked")?;
    Ok(())
}
