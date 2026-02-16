use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tmax_protocol::{
    AgentMessage, AttachMode, Event, MAX_JSON_LINE_BYTES, Request, Response, SessionSummary,
    SharedTask, SharedTaskStatus,
};
use tokio::net::UnixStream;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

struct TestServer {
    _runtime: TempDir,
    socket_path: PathBuf,
    child: Child,
}

impl TestServer {
    async fn spawn() -> Result<Self> {
        Self::spawn_with_args(&[]).await
    }

    async fn spawn_with_args(extra_args: &[&str]) -> Result<Self> {
        let runtime = tempfile::tempdir().context("failed to create temp runtime dir")?;
        let socket_path = runtime.path().join("tmax.sock");

        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("tmax-local"));
        cmd.arg("--socket")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for arg in extra_args {
            cmd.arg(arg);
        }
        let child = cmd.spawn().context("failed to spawn tmax-local")?;

        let start = Instant::now();
        while !socket_path.exists() {
            if start.elapsed() > Duration::from_secs(5) {
                bail!("timed out waiting for socket {}", socket_path.display());
            }
            sleep(Duration::from_millis(20)).await;
        }

        let start = Instant::now();
        loop {
            match UnixStream::connect(&socket_path).await {
                Ok(stream) => {
                    drop(stream);
                    break;
                }
                Err(_) if start.elapsed() <= Duration::from_secs(5) => {
                    sleep(Duration::from_millis(20)).await;
                }
                Err(err) => {
                    bail!(
                        "timed out waiting for server readiness at {}: {err}",
                        socket_path.display()
                    );
                }
            }
        }

        Ok(Self {
            _runtime: runtime,
            socket_path,
            child,
        })
    }

    async fn shutdown(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }

        if let Ok(mut client) = ProtocolClient::connect(&self.socket_path).await {
            let _ = client.request_ok(Request::ServerShutdown).await;
        }

        let start = Instant::now();
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(5) {
                self.child.kill().context("failed to kill tmax-local")?;
                let _ = self.child.wait();
                return Ok(());
            }
            sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct ProtocolClient {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
    writer: FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
}

impl ProtocolClient {
    async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("failed to connect {}", socket_path.display()))?;
        let (read_half, write_half) = stream.into_split();
        let mut client = Self {
            reader: FramedRead::new(
                read_half,
                LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
            ),
            writer: FramedWrite::new(
                write_half,
                LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
            ),
        };

        match client.next_response().await? {
            Response::Hello { .. } => Ok(client),
            other => bail!("expected hello, got {other:?}"),
        }
    }

    async fn send(&mut self, req: Request) -> Result<()> {
        let line = serde_json::to_string(&req)?;
        self.writer.send(line).await?;
        Ok(())
    }

    async fn next_response(&mut self) -> Result<Response> {
        let Some(line) = self.reader.next().await else {
            bail!("server disconnected");
        };
        let line = line?;
        Ok(serde_json::from_str(&line)?)
    }

    async fn request_ok(&mut self, req: Request) -> Result<Option<Value>> {
        self.send(req).await?;
        loop {
            match self.next_response().await? {
                Response::Hello { .. } | Response::Event { .. } => continue,
                Response::Ok { data } => return Ok(data),
                Response::Error { message, .. } => bail!("{message}"),
            }
        }
    }

    async fn attach(&mut self, session_id: &str, mode: AttachMode) -> Result<String> {
        self.send(Request::Attach {
            session_id: session_id.to_string(),
            mode,
            last_seq_seen: None,
        })
        .await?;

        loop {
            match self.next_response().await? {
                Response::Ok { data } => {
                    let attachment_id = data
                        .as_ref()
                        .and_then(|v| v.get("attachment"))
                        .and_then(|v| v.get("attachment_id"))
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("missing attachment id in attach response"))?;
                    return Ok(attachment_id.to_string());
                }
                Response::Hello { .. } | Response::Event { .. } => {}
                Response::Error { message, .. } => bail!("attach failed: {message}"),
            }
        }
    }
}

fn decode_output(data_b64: &str) -> Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::STANDARD.decode(data_b64)?)
}

fn rss_kb(pid: u32) -> Result<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .context("failed to run ps for rss")?;
    if !output.status.success() {
        bail!("ps failed for pid {pid}: status={}", output.status);
    }
    let raw = String::from_utf8(output.stdout)?;
    let rss = raw
        .trim()
        .parse::<u64>()
        .context("failed to parse rss output")?;
    Ok(rss)
}

fn append_session_event_bytes(event: &Event, session_id: &str, out: &mut Vec<u8>) -> Result<()> {
    match event {
        Event::Output {
            session_id: sid,
            data_b64,
            ..
        } if sid == session_id => out.extend_from_slice(&decode_output(data_b64)?),
        Event::Snapshot {
            session_id: sid,
            lines,
            ..
        } if sid == session_id => {
            for line in lines {
                out.extend_from_slice(line.as_bytes());
                out.push(b'\n');
            }
        }
        _ => {}
    }
    Ok(())
}

async fn create_echo_session(client: &mut ProtocolClient, token: &str) -> Result<String> {
    create_shell_session(
        client,
        &format!("printf '{token}'"),
        &format!("session-{token}"),
    )
    .await
}

async fn create_shell_session(
    client: &mut ProtocolClient,
    command: &str,
    label: &str,
) -> Result<String> {
    let data = client
        .request_ok(Request::SessionCreate {
            exec: "/bin/sh".to_string(),
            args: vec!["-lc".to_string(), command.to_string()],
            tags: Vec::new(),
            cwd: None,
            label: Some(label.to_string()),
            sandbox: None,
            parent_id: None,
            cols: 80,
            rows: 24,
        })
        .await?
        .ok_or_else(|| anyhow!("missing session summary"))?;

    let summary: SessionSummary = serde_json::from_value(data)?;
    Ok(summary.session_id)
}

async fn create_shell_session_with_parent(
    client: &mut ProtocolClient,
    command: &str,
    label: &str,
    parent_id: Option<String>,
) -> Result<String> {
    let data = client
        .request_ok(Request::SessionCreate {
            exec: "/bin/sh".to_string(),
            args: vec!["-lc".to_string(), command.to_string()],
            tags: Vec::new(),
            cwd: None,
            label: Some(label.to_string()),
            sandbox: None,
            parent_id,
            cols: 80,
            rows: 24,
        })
        .await?
        .ok_or_else(|| anyhow!("missing session summary"))?;

    let summary: SessionSummary = serde_json::from_value(data)?;
    Ok(summary.session_id)
}

#[tokio::test]
async fn integration_create_and_stream_output() -> Result<()> {
    let mut server = TestServer::spawn().await?;
    let mut control = ProtocolClient::connect(&server.socket_path).await?;
    let session_id = create_echo_session(&mut control, "phase0_integration").await?;

    let mut viewer = ProtocolClient::connect(&server.socket_path).await?;
    let attachment_id = viewer.attach(&session_id, AttachMode::View).await?;

    let output = timeout(Duration::from_secs(5), async {
        let mut combined = Vec::new();
        loop {
            if combined
                .windows("phase0_integration".len())
                .any(|w| w == b"phase0_integration")
            {
                return Ok::<Vec<u8>, anyhow::Error>(combined);
            }
            match viewer.next_response().await? {
                Response::Event { event } => {
                    append_session_event_bytes(event.as_ref(), &session_id, &mut combined)?;
                }
                Response::Error { message, .. } => bail!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .context("timed out waiting for output")??;

    assert!(
        String::from_utf8_lossy(&output).contains("phase0_integration"),
        "expected replayed output, got {:?}",
        String::from_utf8_lossy(&output)
    );

    let _ = viewer
        .request_ok(Request::Detach { attachment_id })
        .await
        .context("failed to detach viewer");
    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id,
            cascade: false,
        })
        .await
        .context("failed to destroy session");
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn integration_single_client_multi_session_subscriptions_preserve_framing() -> Result<()> {
    let mut server = TestServer::spawn().await?;
    let mut control = ProtocolClient::connect(&server.socket_path).await?;

    let session_one =
        create_shell_session(&mut control, "sleep 0.2; printf 'stream_one'", "stream-one").await?;
    let session_two =
        create_shell_session(&mut control, "sleep 0.2; printf 'stream_two'", "stream-two").await?;

    let mut subscriber = ProtocolClient::connect(&server.socket_path).await?;
    subscriber
        .send(Request::Subscribe {
            session_id: session_one.clone(),
            last_seq_seen: None,
        })
        .await?;
    subscriber
        .send(Request::Subscribe {
            session_id: session_two.clone(),
            last_seq_seen: None,
        })
        .await?;

    timeout(Duration::from_secs(5), async {
        let mut seen_ack = 0usize;
        let mut out_one = Vec::new();
        let mut out_two = Vec::new();

        loop {
            if seen_ack >= 2
                && String::from_utf8_lossy(&out_one).contains("stream_one")
                && String::from_utf8_lossy(&out_two).contains("stream_two")
            {
                break Ok::<(), anyhow::Error>(());
            }

            match subscriber.next_response().await? {
                Response::Ok { data } => {
                    if data
                        .as_ref()
                        .and_then(|v| v.get("subscribed"))
                        .and_then(|v| v.as_bool())
                        == Some(true)
                    {
                        seen_ack += 1;
                    }
                }
                Response::Event { event } => {
                    append_session_event_bytes(event.as_ref(), &session_one, &mut out_one)?;
                    append_session_event_bytes(event.as_ref(), &session_two, &mut out_two)?;
                }
                Response::Error { message, .. } => bail!("{message}"),
                Response::Hello { .. } => {}
            }
        }
    })
    .await
    .context("timed out waiting for both subscription streams")??;

    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id: session_one,
            cascade: false,
        })
        .await;
    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id: session_two,
            cascade: false,
        })
        .await;

    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn integration_socket_path_is_mode_0600() -> Result<()> {
    let mut server = TestServer::spawn().await?;
    let mode = timeout(Duration::from_secs(2), async {
        loop {
            let mode = fs::metadata(&server.socket_path)?.permissions().mode() & 0o777;
            if mode == 0o600 {
                return Ok::<u32, anyhow::Error>(mode);
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("timed out waiting for socket chmod to 0600")??;
    assert_eq!(mode, 0o600, "socket mode must be 0600");
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn integration_multiple_clients_subscribe_same_session() -> Result<()> {
    let mut server = TestServer::spawn().await?;
    let mut control = ProtocolClient::connect(&server.socket_path).await?;
    let session_id = create_echo_session(&mut control, "multi_subscribers").await?;

    async fn subscribe_replay(socket: &Path, session_id: &str, token: &str) -> Result<Vec<u8>> {
        let mut client = ProtocolClient::connect(socket).await?;
        client
            .send(Request::Subscribe {
                session_id: session_id.to_string(),
                last_seq_seen: None,
            })
            .await?;

        timeout(Duration::from_secs(5), async {
            let mut out = Vec::new();
            loop {
                if String::from_utf8_lossy(&out).contains(token) {
                    return Ok::<Vec<u8>, anyhow::Error>(out);
                }
                match client.next_response().await? {
                    Response::Event { event } => {
                        append_session_event_bytes(event.as_ref(), session_id, &mut out)?;
                    }
                    Response::Ok { .. } | Response::Hello { .. } => {}
                    Response::Error { message, .. } => bail!("{message}"),
                }
            }
        })
        .await
        .context("timed out waiting for replay/live output")?
    }

    let left = subscribe_replay(&server.socket_path, &session_id, "multi_subscribers").await?;
    let right = subscribe_replay(&server.socket_path, &session_id, "multi_subscribers").await?;

    assert!(
        String::from_utf8_lossy(&left).contains("multi_subscribers"),
        "expected replay output, got {:?}",
        String::from_utf8_lossy(&left)
    );
    assert!(
        String::from_utf8_lossy(&right).contains("multi_subscribers"),
        "expected replay output, got {:?}",
        String::from_utf8_lossy(&right)
    );

    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id,
            cascade: false,
        })
        .await
        .context("failed to destroy session");
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn integration_edit_vs_view_attachment_enforcement() -> Result<()> {
    let mut server = TestServer::spawn().await?;
    let mut control = ProtocolClient::connect(&server.socket_path).await?;

    let created = control
        .request_ok(Request::SessionCreate {
            exec: "/bin/cat".to_string(),
            args: Vec::new(),
            tags: Vec::new(),
            cwd: None,
            label: Some("cat".to_string()),
            sandbox: None,
            parent_id: None,
            cols: 80,
            rows: 24,
        })
        .await?
        .ok_or_else(|| anyhow!("missing session summary"))?;
    let summary: SessionSummary = serde_json::from_value(created)?;
    let session_id = summary.session_id;

    let mut view_client = ProtocolClient::connect(&server.socket_path).await?;
    let view_attachment = view_client.attach(&session_id, AttachMode::View).await?;
    let mut edit_client = ProtocolClient::connect(&server.socket_path).await?;
    let edit_attachment = edit_client.attach(&session_id, AttachMode::Edit).await?;

    view_client
        .send(Request::SendInput {
            session_id: session_id.clone(),
            attachment_id: view_attachment.clone(),
            data_b64: base64::engine::general_purpose::STANDARD.encode("blocked"),
        })
        .await?;
    let denied = timeout(Duration::from_secs(5), async {
        loop {
            match view_client.next_response().await? {
                Response::Error { message, .. } => return Ok::<String, anyhow::Error>(message),
                Response::Event { .. } | Response::Hello { .. } | Response::Ok { .. } => {}
            }
        }
    })
    .await
    .context("timed out waiting for view-attachment denial")??;
    assert!(
        denied.contains("view-only") || denied.contains("input denied"),
        "unexpected error message: {denied}"
    );

    edit_client
        .send(Request::SendInput {
            session_id: session_id.clone(),
            attachment_id: edit_attachment.clone(),
            data_b64: base64::engine::general_purpose::STANDARD.encode("echo_from_edit\n"),
        })
        .await?;
    timeout(Duration::from_secs(5), async {
        loop {
            match edit_client.next_response().await? {
                Response::Ok { .. } => break Ok::<(), anyhow::Error>(()),
                Response::Error { message, .. } => bail!("{message}"),
                Response::Event { .. } | Response::Hello { .. } => {}
            }
        }
    })
    .await
    .context("timed out waiting for edit-input ack")??;

    let echoed = timeout(Duration::from_secs(5), async {
        let mut out = Vec::new();
        loop {
            if String::from_utf8_lossy(&out).contains("echo_from_edit") {
                return Ok::<Vec<u8>, anyhow::Error>(out);
            }
            match edit_client.next_response().await? {
                Response::Event { event } => {
                    if let Event::Output {
                        session_id: sid,
                        data_b64,
                        ..
                    } = *event
                        && sid == session_id
                    {
                        out.extend_from_slice(&decode_output(&data_b64)?);
                    }
                }
                Response::Error { message, .. } => bail!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .context("timed out waiting for cat echo")??;

    assert!(String::from_utf8_lossy(&echoed).contains("echo_from_edit"));

    let _ = view_client
        .request_ok(Request::Detach {
            attachment_id: view_attachment,
        })
        .await;
    let _ = edit_client
        .request_ok(Request::Detach {
            attachment_id: edit_attachment,
        })
        .await;
    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id,
            cascade: false,
        })
        .await;

    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn integration_perf_smoke_create_and_stream_latency() -> Result<()> {
    let mut server = TestServer::spawn().await?;
    let mut control = ProtocolClient::connect(&server.socket_path).await?;
    let mut viewer = ProtocolClient::connect(&server.socket_path).await?;
    let mut editor = ProtocolClient::connect(&server.socket_path).await?;
    let baseline_rss_kb = rss_kb(server.child.id())?;

    let create_start = Instant::now();
    let session_id = create_shell_session(&mut control, "cat", "perf-cat").await?;
    let create_elapsed = create_start.elapsed();
    let rss_after_create_kb = rss_kb(server.child.id())?;
    let per_session_rss_kb = rss_after_create_kb.saturating_sub(baseline_rss_kb);

    let _view_attachment = viewer.attach(&session_id, AttachMode::View).await?;
    let edit_attachment = editor.attach(&session_id, AttachMode::Edit).await?;

    let token = "perf_smoke_token_123";
    let send_start = Instant::now();
    editor
        .send(Request::SendInput {
            session_id: session_id.clone(),
            attachment_id: edit_attachment.clone(),
            data_b64: base64::engine::general_purpose::STANDARD.encode(format!("{token}\n")),
        })
        .await?;

    timeout(Duration::from_secs(5), async {
        loop {
            match editor.next_response().await? {
                Response::Ok { .. } => break Ok::<(), anyhow::Error>(()),
                Response::Error { message, .. } => bail!("{message}"),
                Response::Event { .. } | Response::Hello { .. } => {}
            }
        }
    })
    .await
    .context("timed out waiting for send-input ack in perf smoke")??;

    timeout(Duration::from_secs(5), async {
        let mut out = Vec::new();
        loop {
            if String::from_utf8_lossy(&out).contains(token) {
                break Ok::<(), anyhow::Error>(());
            }
            match viewer.next_response().await? {
                Response::Event { event } => {
                    append_session_event_bytes(event.as_ref(), &session_id, &mut out)?;
                }
                Response::Error { message, .. } => bail!("{message}"),
                Response::Hello { .. } | Response::Ok { .. } => {}
            }
        }
    })
    .await
    .context("timed out waiting for perf smoke output")??;
    let stream_elapsed = send_start.elapsed();

    eprintln!(
        "perf_smoke create_ms={} stream_ms={} rss_delta_kb={}",
        create_elapsed.as_millis(),
        stream_elapsed.as_millis(),
        per_session_rss_kb
    );

    assert!(
        create_elapsed <= Duration::from_millis(500),
        "session create latency too high for smoke gate: {}ms",
        create_elapsed.as_millis()
    );
    assert!(
        stream_elapsed <= Duration::from_millis(200),
        "output stream latency too high for smoke gate: {}ms",
        stream_elapsed.as_millis()
    );
    assert!(
        per_session_rss_kb <= 7 * 1024,
        "per-session rss too high for smoke gate (target is 5MB): {}KB",
        per_session_rss_kb
    );

    let _ = editor
        .request_ok(Request::Detach {
            attachment_id: edit_attachment,
        })
        .await;
    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id,
            cascade: false,
        })
        .await;

    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn integration_messages_and_shared_tasks_round_trip() -> Result<()> {
    let mut server = TestServer::spawn().await?;
    let mut control = ProtocolClient::connect(&server.socket_path).await?;

    let sender = create_shell_session(&mut control, "sleep 3", "sender").await?;
    let recipient = create_shell_session(&mut control, "sleep 3", "recipient").await?;

    let sent_message = control
        .request_ok(Request::MessageSend {
            from_session_id: Some(sender.clone()),
            to_session_id: recipient.clone(),
            topic: Some("question".to_string()),
            body: "Do we target API v1 or v2?".to_string(),
            requires_response: true,
            encrypt: false,
            sign: false,
        })
        .await?
        .ok_or_else(|| anyhow!("missing message send payload"))?;
    let sent_message: AgentMessage = serde_json::from_value(sent_message)?;
    assert_eq!(sent_message.to_session_id, recipient);

    let unread_count = control
        .request_ok(Request::MessageUnreadCount {
            session_id: recipient.clone(),
        })
        .await?
        .ok_or_else(|| anyhow!("missing unread count payload"))?;
    assert_eq!(unread_count.get("unread").and_then(Value::as_u64), Some(1));

    let messages = control
        .request_ok(Request::MessageList {
            session_id: recipient.clone(),
            unread_only: true,
            limit: None,
        })
        .await?
        .ok_or_else(|| anyhow!("missing message list payload"))?;
    let messages: Vec<AgentMessage> = serde_json::from_value(messages)?;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message_id, sent_message.message_id);

    let acked = control
        .request_ok(Request::MessageAck {
            session_id: recipient.clone(),
            message_id: sent_message.message_id.clone(),
        })
        .await?
        .ok_or_else(|| anyhow!("missing message ack payload"))?;
    let acked: AgentMessage = serde_json::from_value(acked)?;
    assert!(acked.read_at_ms.is_some(), "message should be marked read");

    let workflow = control
        .request_ok(Request::WorkflowCreate {
            name: "analysis".to_string(),
            root_session_id: sender.clone(),
        })
        .await?
        .ok_or_else(|| anyhow!("missing workflow payload"))?;
    let workflow_id = workflow
        .get("workflow_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing workflow_id"))?
        .to_string();
    let _ = control
        .request_ok(Request::WorkflowJoin {
            workflow_id: workflow_id.clone(),
            session_id: recipient.clone(),
            parent_session_id: sender.clone(),
        })
        .await?;

    let parent_task = control
        .request_ok(Request::TaskCreate {
            workflow_id: workflow_id.clone(),
            title: "Collect evidence".to_string(),
            description: None,
            created_by: sender.clone(),
            depends_on: Vec::new(),
        })
        .await?
        .ok_or_else(|| anyhow!("missing parent task payload"))?;
    let parent_task: SharedTask = serde_json::from_value(parent_task)?;
    assert_eq!(parent_task.status, SharedTaskStatus::Todo);

    let child_task = control
        .request_ok(Request::TaskCreate {
            workflow_id: workflow_id.clone(),
            title: "Write recommendation".to_string(),
            description: Some("depends on evidence".to_string()),
            created_by: sender.clone(),
            depends_on: vec![parent_task.task_id.clone()],
        })
        .await?
        .ok_or_else(|| anyhow!("missing child task payload"))?;
    let child_task: SharedTask = serde_json::from_value(child_task)?;
    assert_eq!(child_task.status, SharedTaskStatus::Blocked);

    let claimed_blocked = control
        .request_ok(Request::TaskClaim {
            task_id: child_task.task_id.clone(),
            session_id: recipient.clone(),
        })
        .await?
        .ok_or_else(|| anyhow!("missing task claim payload"))?;
    let claimed_blocked: SharedTask = serde_json::from_value(claimed_blocked)?;
    assert_eq!(claimed_blocked.status, SharedTaskStatus::Blocked);

    let _ = control
        .request_ok(Request::TaskSetStatus {
            task_id: parent_task.task_id.clone(),
            session_id: sender.clone(),
            status: SharedTaskStatus::Done,
        })
        .await?;
    let claimed_ready = control
        .request_ok(Request::TaskClaim {
            task_id: child_task.task_id.clone(),
            session_id: recipient.clone(),
        })
        .await?
        .ok_or_else(|| anyhow!("missing claimed ready payload"))?;
    let claimed_ready: SharedTask = serde_json::from_value(claimed_ready)?;
    assert_eq!(claimed_ready.status, SharedTaskStatus::InProgress);

    let _ = control
        .request_ok(Request::TaskSetStatus {
            task_id: child_task.task_id.clone(),
            session_id: recipient.clone(),
            status: SharedTaskStatus::Done,
        })
        .await?;
    let open_tasks = control
        .request_ok(Request::TaskList {
            workflow_id: workflow_id.clone(),
            session_id: sender.clone(),
            include_done: false,
        })
        .await?
        .ok_or_else(|| anyhow!("missing task list payload"))?;
    let open_tasks: Vec<SharedTask> = serde_json::from_value(open_tasks)?;
    assert!(open_tasks.is_empty(), "expected all tasks to be done");

    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id: sender,
            cascade: false,
        })
        .await;
    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id: recipient,
            cascade: false,
        })
        .await;
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn integration_attached_connections_are_session_aware_for_messages() -> Result<()> {
    let mut server = TestServer::spawn().await?;
    let mut control = ProtocolClient::connect(&server.socket_path).await?;
    let mut observer = ProtocolClient::connect(&server.socket_path).await?;

    let sender = create_shell_session(&mut control, "sleep 3", "sender-bound").await?;
    let recipient = create_shell_session(&mut control, "sleep 3", "recipient-bound").await?;
    let other = create_shell_session(&mut control, "sleep 3", "other-bound").await?;

    let sender_attachment = control.attach(&sender, AttachMode::View).await?;

    let sent = control
        .request_ok(Request::MessageSend {
            from_session_id: None,
            to_session_id: recipient.clone(),
            topic: Some("question".to_string()),
            body: "bound-inference".to_string(),
            requires_response: false,
            encrypt: false,
            sign: false,
        })
        .await?
        .ok_or_else(|| anyhow!("missing sent message payload"))?;
    let sent: AgentMessage = serde_json::from_value(sent)?;
    assert_eq!(
        sent.from_session_id.as_deref(),
        Some(sender.as_str()),
        "sender should be inferred from attached session"
    );

    let denied = control
        .request_ok(Request::MessageList {
            session_id: recipient.clone(),
            unread_only: false,
            limit: None,
        })
        .await;
    assert!(
        denied.is_err(),
        "attached connection should not access another session inbox"
    );

    let allowed_self = control
        .request_ok(Request::MessageList {
            session_id: sender.clone(),
            unread_only: false,
            limit: None,
        })
        .await?;
    let allowed_self = allowed_self.unwrap_or_else(|| serde_json::json!([]));
    let allowed_self: Vec<AgentMessage> = serde_json::from_value(allowed_self)?;
    assert!(allowed_self.is_empty(), "sender inbox should be empty");

    let recipient_messages = observer
        .request_ok(Request::MessageList {
            session_id: recipient.clone(),
            unread_only: false,
            limit: None,
        })
        .await?
        .ok_or_else(|| anyhow!("missing recipient inbox payload"))?;
    let recipient_messages: Vec<AgentMessage> = serde_json::from_value(recipient_messages)?;
    assert_eq!(recipient_messages.len(), 1);
    assert_eq!(recipient_messages[0].body, "bound-inference");

    let _ = control
        .request_ok(Request::Detach {
            attachment_id: sender_attachment,
        })
        .await;
    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id: sender,
            cascade: false,
        })
        .await;
    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id: recipient,
            cascade: false,
        })
        .await;
    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id: other,
            cascade: false,
        })
        .await;
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn integration_parent_only_policy_denies_sibling_routes() -> Result<()> {
    let mut server = TestServer::spawn_with_args(&["--comms-policy", "parent_only"]).await?;
    let mut control = ProtocolClient::connect(&server.socket_path).await?;

    let root =
        create_shell_session_with_parent(&mut control, "sleep 3", "root-policy", None).await?;
    let child_a = create_shell_session_with_parent(
        &mut control,
        "sleep 3",
        "child-a-policy",
        Some(root.clone()),
    )
    .await?;
    let child_b = create_shell_session_with_parent(
        &mut control,
        "sleep 3",
        "child-b-policy",
        Some(root.clone()),
    )
    .await?;

    let sibling_route = control
        .request_ok(Request::MessageSend {
            from_session_id: Some(child_a.clone()),
            to_session_id: child_b.clone(),
            topic: Some("question".to_string()),
            body: "Sibling route should be blocked".to_string(),
            requires_response: false,
            encrypt: false,
            sign: false,
        })
        .await;
    assert!(
        sibling_route.is_err(),
        "parent_only policy should deny sibling message routes"
    );

    let parent_route = control
        .request_ok(Request::MessageSend {
            from_session_id: Some(child_a.clone()),
            to_session_id: root.clone(),
            topic: Some("question".to_string()),
            body: "Parent route should be allowed".to_string(),
            requires_response: false,
            encrypt: false,
            sign: false,
        })
        .await?;
    assert!(
        parent_route.is_some(),
        "expected parent route to be allowed"
    );

    let workflow = control
        .request_ok(Request::WorkflowCreate {
            name: "policy-tree".to_string(),
            root_session_id: root.clone(),
        })
        .await?
        .ok_or_else(|| anyhow!("missing workflow payload"))?;
    let workflow_id = workflow
        .get("workflow_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing workflow_id"))?
        .to_string();
    let _ = control
        .request_ok(Request::WorkflowJoin {
            workflow_id: workflow_id.clone(),
            session_id: child_a.clone(),
            parent_session_id: root.clone(),
        })
        .await?;
    let _ = control
        .request_ok(Request::WorkflowJoin {
            workflow_id: workflow_id.clone(),
            session_id: child_b.clone(),
            parent_session_id: root.clone(),
        })
        .await?;

    let parent_task = control
        .request_ok(Request::TaskCreate {
            workflow_id: workflow_id.clone(),
            title: "Top-down task".to_string(),
            description: None,
            created_by: child_a.clone(),
            depends_on: Vec::new(),
        })
        .await?
        .ok_or_else(|| anyhow!("missing parent task payload"))?;
    let parent_task: SharedTask = serde_json::from_value(parent_task)?;

    let sibling_claim = control
        .request_ok(Request::TaskClaim {
            task_id: parent_task.task_id.clone(),
            session_id: child_b.clone(),
        })
        .await;
    assert!(
        sibling_claim.is_err(),
        "parent_only policy should deny sibling task claim"
    );

    let parent_claim = control
        .request_ok(Request::TaskClaim {
            task_id: parent_task.task_id.clone(),
            session_id: root.clone(),
        })
        .await?;
    assert!(
        parent_claim.is_some(),
        "expected parent claim to be allowed"
    );

    let _ = control
        .request_ok(Request::SessionDestroy {
            session_id: root,
            cascade: true,
        })
        .await;
    server.shutdown().await?;
    Ok(())
}
