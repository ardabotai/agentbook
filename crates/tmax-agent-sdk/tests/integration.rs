use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tmax_agent_sdk::{
    AgentClient, AgentSdkError, DeployOptions, ExecutionOptions, RollbackOptions, RunTaskOptions,
    execute_task_and_collect, run_deploy, run_rollback, wait_ready,
};
use tmax_protocol::{Request, Response, SessionSummary, SharedTaskStatus};

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

fn spawn_mock_server<F>(socket_path: PathBuf, handler: F) -> thread::JoinHandle<Result<()>>
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

        let listener = UnixListener::bind(&socket_path).context("bind failed")?;
        let (stream, _) = listener.accept().context("accept failed")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("set read timeout failed")?;

        let read_half = stream.try_clone().context("clone stream failed")?;
        let mut reader = BufReader::new(read_half);
        let mut writer = BufWriter::new(stream);

        write_response(&mut writer, &Response::hello(vec!["test".to_string()]))?;
        handler(&mut reader, &mut writer)
    })
}

fn wait_for_socket(path: &std::path::Path) -> Result<()> {
    for _ in 0..100 {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("socket did not appear: {}", path.display())
}

fn sample_summary() -> SessionSummary {
    SessionSummary {
        session_id: "s-task".to_string(),
        label: Some("task".to_string()),
        tags: vec!["agent".to_string()],
        exec: "/bin/echo".to_string(),
        cwd: std::env::current_dir().expect("cwd"),
        parent_id: None,
        created_at_ms: 1,
        sandboxed: false,
        git_branch: None,
        git_repo_root: None,
        git_worktree_path: None,
        git_dirty: None,
    }
}

#[tokio::test]
async fn run_task_streams_output_and_waits_for_exit() -> Result<()> {
    let temp = tempfile::tempdir().context("tempdir")?;
    let socket = temp.path().join("sdk-run.sock");

    let summary = sample_summary();
    let server = spawn_mock_server(socket.clone(), move |reader, writer| {
        match read_request(reader)? {
            Request::SessionCreate { exec, .. } => {
                if exec != "/bin/echo" {
                    bail!("unexpected exec: {exec}");
                }
            }
            other => bail!("expected SessionCreate, got {other:?}"),
        }

        write_response(writer, &Response::ok(Some(serde_json::to_value(&summary)?)))?;

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

        write_response(
            writer,
            &Response::ok(Some(serde_json::json!({"subscribed": true}))),
        )?;
        write_response(
            writer,
            &Response::Event {
                event: Box::new(tmax_protocol::Event::Output {
                    session_id: "s-task".to_string(),
                    seq: 1,
                    data_b64: "aGVsbG8K".to_string(),
                }),
            },
        )?;
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

    wait_for_socket(&socket)?;
    let mut client = AgentClient::connect(&socket).await?;
    let mut output = Vec::new();
    let result = client
        .run_task(
            RunTaskOptions {
                exec: "/bin/echo".to_string(),
                args: vec!["hello".to_string()],
                tags: vec!["agent".to_string()],
                cwd: None,
                label: Some("task".to_string()),
                sandbox: None,
                parent_id: None,
                cols: 80,
                rows: 24,
                last_seq_seen: Some(0),
            },
            |chunk| {
                output.extend_from_slice(chunk);
                Ok(())
            },
        )
        .await?;

    assert_eq!(result.session.session_id, "s-task");
    assert!(result.succeeded());
    assert_eq!(output, b"hello\n");

    server.join().expect("server thread panicked")?;
    Ok(())
}

#[tokio::test]
async fn cancel_task_sends_destroy_request() -> Result<()> {
    let temp = tempfile::tempdir().context("tempdir")?;
    let socket = temp.path().join("sdk-cancel.sock");

    let server = spawn_mock_server(socket.clone(), move |reader, writer| {
        match read_request(reader)? {
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
            }
            other => bail!("expected SessionDestroy, got {other:?}"),
        }

        write_response(writer, &Response::ok(Some(serde_json::json!({"ok": true}))))?;
        Ok(())
    });

    wait_for_socket(&socket)?;
    let mut client = AgentClient::connect(&socket).await?;
    client.cancel_task("s-cancel", true).await?;

    server.join().expect("server thread panicked")?;
    Ok(())
}

#[tokio::test]
async fn execute_task_and_collect_returns_buffered_output() -> Result<()> {
    let temp = tempfile::tempdir().context("tempdir")?;
    let socket = temp.path().join("sdk-exec.sock");

    let summary = sample_summary();
    let socket_for_thread = socket.clone();
    let server = thread::spawn(move || -> Result<()> {
        if socket_for_thread.exists() {
            let _ = fs::remove_file(&socket_for_thread);
        }
        let listener = UnixListener::bind(&socket_for_thread).context("bind failed")?;

        let (stream_a, _) = listener.accept().context("accept A failed")?;
        stream_a
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("set read timeout A failed")?;
        let read_a = stream_a.try_clone().context("clone A failed")?;
        let mut reader_a = BufReader::new(read_a);
        let mut writer_a = BufWriter::new(stream_a);
        write_response(&mut writer_a, &Response::hello(vec!["test".to_string()]))?;
        match read_request(&mut reader_a)? {
            Request::SessionCreate { .. } => {}
            other => bail!("expected SessionCreate, got {other:?}"),
        }
        write_response(
            &mut writer_a,
            &Response::ok(Some(serde_json::to_value(&summary)?)),
        )?;
        drop(writer_a);

        let (stream_b, _) = listener.accept().context("accept B failed")?;
        stream_b
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("set read timeout B failed")?;
        let read_b = stream_b.try_clone().context("clone B failed")?;
        let mut reader_b = BufReader::new(read_b);
        let mut writer_b = BufWriter::new(stream_b);
        write_response(&mut writer_b, &Response::hello(vec!["test".to_string()]))?;
        match read_request(&mut reader_b)? {
            Request::Subscribe { session_id, .. } => {
                if session_id != "s-task" {
                    bail!("unexpected session id: {session_id}");
                }
            }
            other => bail!("expected Subscribe, got {other:?}"),
        }
        write_response(
            &mut writer_b,
            &Response::ok(Some(serde_json::json!({"subscribed": true}))),
        )?;
        write_response(
            &mut writer_b,
            &Response::Event {
                event: Box::new(tmax_protocol::Event::Output {
                    session_id: "s-task".to_string(),
                    seq: 1,
                    data_b64: "Y29sbGVjdGVkCg==".to_string(),
                }),
            },
        )?;
        write_response(
            &mut writer_b,
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

    wait_for_socket(&socket)?;
    let collected = execute_task_and_collect(
        &socket,
        RunTaskOptions {
            exec: "/bin/echo".to_string(),
            args: vec!["collected".to_string()],
            tags: vec!["agent".to_string()],
            cwd: None,
            label: Some("task".to_string()),
            sandbox: None,
            parent_id: None,
            cols: 80,
            rows: 24,
            last_seq_seen: Some(0),
        },
        ExecutionOptions::default(),
    )
    .await?;

    assert!(collected.run.succeeded());
    assert_eq!(collected.output_utf8_lossy(), "collected\n");

    server.join().expect("server thread panicked")?;
    Ok(())
}

#[tokio::test]
async fn wait_ready_times_out_when_socket_never_appears() -> Result<()> {
    let temp = tempfile::tempdir().context("tempdir")?;
    let socket = temp.path().join("never.sock");

    let err = wait_ready(
        &socket,
        Duration::from_millis(60),
        Duration::from_millis(10),
    )
    .await
    .expect_err("wait_ready should time out");

    match err {
        AgentSdkError::Timeout { operation, .. } => assert_eq!(operation, "wait_ready"),
        other => bail!("unexpected error type: {other}"),
    }
    Ok(())
}

#[tokio::test]
async fn deploy_and_rollback_wrappers_run_custom_scripts() -> Result<()> {
    let temp = tempfile::tempdir().context("tempdir")?;
    let deploy_script = temp.path().join("deploy.sh");
    let rollback_script = temp.path().join("rollback.sh");

    fs::write(
        &deploy_script,
        "#!/usr/bin/env bash\nset -euo pipefail\necho \"DEPLOY:$*\"\n",
    )?;
    fs::write(
        &rollback_script,
        "#!/usr/bin/env bash\nset -euo pipefail\necho \"ROLLBACK:$*\"\n",
    )?;
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&deploy_script)
        .arg(&rollback_script)
        .status()
        .context("chmod failed")?;

    let mut deploy = DeployOptions::new(temp.path().join("artifact.tgz"));
    deploy.script_path = deploy_script;
    deploy.dry_run = true;
    let deploy_out = run_deploy(deploy).await?;
    assert!(deploy_out.stdout.contains("DEPLOY:"));
    assert!(deploy_out.stdout.contains("--artifact"));

    let rollback = RollbackOptions {
        script_path: rollback_script,
        dry_run: true,
        ..RollbackOptions::default()
    };
    let rollback_out = run_rollback(rollback).await?;
    assert!(rollback_out.stdout.contains("ROLLBACK:"));
    assert!(rollback_out.stdout.contains("--dry-run"));

    Ok(())
}

#[tokio::test]
async fn rollback_wrapper_surfaces_command_failures() -> Result<()> {
    let temp = tempfile::tempdir().context("tempdir")?;
    let rollback_script = temp.path().join("rollback-fail.sh");
    fs::write(
        &rollback_script,
        "#!/usr/bin/env bash\nset -euo pipefail\necho \"boom\" 1>&2\nexit 7\n",
    )?;
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&rollback_script)
        .status()
        .context("chmod failed")?;

    let err = run_rollback(RollbackOptions {
        script_path: rollback_script,
        ..RollbackOptions::default()
    })
    .await
    .expect_err("rollback should fail");

    match err {
        AgentSdkError::CommandFailed { status, stderr, .. } => {
            assert_eq!(status, Some(7));
            assert!(stderr.contains("boom"));
        }
        other => bail!("unexpected error type: {other}"),
    }
    Ok(())
}

#[tokio::test]
async fn message_and_shared_task_helpers_round_trip() -> Result<()> {
    let temp = tempfile::tempdir().context("tempdir")?;
    let socket = temp.path().join("sdk-message-task.sock");

    let server = spawn_mock_server(socket.clone(), move |reader, writer| {
        match read_request(reader)? {
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
                    bail!("unexpected from session: {from_session_id:?}");
                }
                if to_session_id != "child" {
                    bail!("unexpected to session: {to_session_id}");
                }
                if topic.as_deref() != Some("question") {
                    bail!("unexpected topic: {topic:?}");
                }
                if body != "Need an answer" {
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
            }
            other => bail!("expected MessageSend, got {other:?}"),
        }
        write_response(
            writer,
            &Response::ok(Some(serde_json::json!({
                "message_id": "m1",
                "from_session_id": "root",
                "to_session_id": "child",
                "topic": "question",
                "body": "Need an answer",
                "requires_response": true,
                "created_at_ms": 1,
                "read_at_ms": null
            }))),
        )?;

        match read_request(reader)? {
            Request::TaskSetStatus {
                task_id,
                session_id,
                status,
            } => {
                if task_id != "task-1" {
                    bail!("unexpected task id: {task_id}");
                }
                if session_id != "child" {
                    bail!("unexpected session id: {session_id}");
                }
                if status != SharedTaskStatus::Done {
                    bail!("unexpected status: {status:?}");
                }
            }
            other => bail!("expected TaskSetStatus, got {other:?}"),
        }
        write_response(
            writer,
            &Response::ok(Some(serde_json::json!({
                "task_id": "task-1",
                "workflow_id": "wf-1",
                "title": "Task",
                "description": null,
                "status": "done",
                "created_by": null,
                "assignee_session_id": "child",
                "depends_on": [],
                "created_at_ms": 1,
                "updated_at_ms": 2,
                "completed_at_ms": 2
            }))),
        )?;
        Ok(())
    });

    wait_for_socket(&socket)?;
    let mut client = AgentClient::connect(&socket).await?;
    let message = client
        .send_message(
            Some("root".to_string()),
            "child".to_string(),
            Some("question".to_string()),
            "Need an answer".to_string(),
            true,
            false,
            false,
        )
        .await?;
    assert_eq!(message.message_id, "m1");

    let task = client
        .set_shared_task_status("task-1", "child", SharedTaskStatus::Done)
        .await?;
    assert_eq!(task.status, SharedTaskStatus::Done);

    server.join().expect("server thread panicked")?;
    Ok(())
}
