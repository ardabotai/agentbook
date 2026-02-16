use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use clap::{ArgAction, Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use nix::unistd::Uid;
use serde_json::json;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tmax_agent_sdk::{
    AgentClient, AgentSdkError, ExecutionOptions, RetryPolicy, RunTaskOptions,
    execute_task_and_collect, tail_task_resumable,
};
use tmax_protocol::{
    AttachMode, MAX_JSON_LINE_BYTES, MeshMessageType, Request, Response, SandboxConfig,
    SessionSummary, SharedTaskStatus, TrustTier,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

#[derive(Parser, Debug)]
#[command(name = "tmax")]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: CommandGroup,
}

#[derive(Subcommand, Debug)]
enum CommandGroup {
    Server {
        #[command(subcommand)]
        cmd: ServerCmd,
    },
    New {
        #[arg(long)]
        exec: Option<String>,
        #[arg(long, action = ArgAction::SetTrue)]
        shell: bool,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long = "sandbox-write")]
        sandbox_write: Vec<PathBuf>,
        #[arg(long, action = ArgAction::SetTrue)]
        no_sandbox: bool,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value_t = 80)]
        cols: u16,
        #[arg(long, default_value_t = 24)]
        rows: u16,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    List {
        #[arg(long, action = ArgAction::SetTrue)]
        tree: bool,
    },
    Info {
        session: String,
    },
    Attach {
        session: String,
        #[arg(long, action = ArgAction::SetTrue)]
        view: bool,
        #[arg(long)]
        last_seq: Option<u64>,
    },
    Detach {
        attachment: String,
    },
    Send {
        session: String,
        input: String,
        #[arg(long)]
        attachment: String,
    },
    Resize {
        session: String,
        cols: u16,
        rows: u16,
    },
    Kill {
        session: String,
        #[arg(long, action = ArgAction::SetTrue)]
        cascade: bool,
    },
    Marker {
        session: String,
        name: String,
    },
    Markers {
        session: String,
    },
    Stream {
        session: String,
        #[arg(long)]
        last_seq: Option<u64>,
    },
    Subscribe {
        session: String,
        #[arg(long)]
        last_seq: Option<u64>,
    },
    RunTask {
        #[arg(long)]
        exec: Option<String>,
        #[arg(long, action = ArgAction::SetTrue)]
        shell: bool,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long = "sandbox-write")]
        sandbox_write: Vec<PathBuf>,
        #[arg(long, action = ArgAction::SetTrue)]
        no_sandbox: bool,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value_t = 80)]
        cols: u16,
        #[arg(long, default_value_t = 24)]
        rows: u16,
        #[arg(long)]
        last_seq: Option<u64>,
        #[arg(long, action = ArgAction::SetTrue)]
        json: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        no_stream: bool,
        #[arg(long)]
        timeout_ms: Option<u64>,
        #[arg(long, default_value_t = 3)]
        retry_attempts: u32,
        #[arg(long, default_value_t = 100)]
        retry_base_ms: u64,
        #[arg(long, default_value_t = 2000)]
        retry_max_ms: u64,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    TailTask {
        session: String,
        #[arg(long)]
        last_seq: Option<u64>,
        #[arg(long, action = ArgAction::SetTrue)]
        json: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        no_stream: bool,
        #[arg(long, default_value_t = 3)]
        retry_attempts: u32,
        #[arg(long, default_value_t = 100)]
        retry_base_ms: u64,
        #[arg(long, default_value_t = 2000)]
        retry_max_ms: u64,
    },
    CancelTask {
        session: String,
        #[arg(long, action = ArgAction::SetTrue)]
        cascade: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        json: bool,
    },
    Health {
        #[arg(long, action = ArgAction::SetTrue)]
        json: bool,
    },
    Msg {
        #[command(subcommand)]
        cmd: MsgCmd,
    },
    Tasks {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    Workflows {
        #[command(subcommand)]
        cmd: WorkflowCmd,
    },
    Worktree {
        #[command(subcommand)]
        cmd: WorktreeCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Invite {
        #[command(subcommand)]
        cmd: InviteCmd,
    },
    Friends {
        #[command(subcommand)]
        cmd: FriendsCmd,
    },
    Inbox {
        #[command(subcommand)]
        cmd: InboxCmd,
    },
    Remote {
        #[command(subcommand)]
        cmd: RemoteCmd,
    },
    /// Start tmax (tmax-local by default, or tmax-node with --node)
    Up {
        /// Start tmax-node instead of tmax-local, with --peer-listen on 0.0.0.0:50052
        #[arg(long, action = ArgAction::SetTrue)]
        node: bool,
        /// Run in foreground (don't daemonize)
        #[arg(long, action = ArgAction::SetTrue)]
        foreground: bool,
        /// Peer listen address (only with --node, default 0.0.0.0:50052)
        #[arg(long, default_value = "0.0.0.0:50052")]
        peer_listen: String,
        /// Relay host addresses (only with --node, repeatable)
        #[arg(long)]
        relay_host: Vec<String>,
    },
    /// Stop tmax
    Down,
}

#[derive(Subcommand, Debug)]
enum ServerCmd {
    Start {
        #[arg(long, action = ArgAction::SetTrue)]
        foreground: bool,
    },
    Stop,
    Status,
}

#[derive(Subcommand, Debug)]
enum WorktreeCmd {
    Clean { session: String },
}

#[derive(Subcommand, Debug)]
enum MsgCmd {
    Send {
        #[arg(long)]
        to: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        topic: Option<String>,
        #[arg(long, action = ArgAction::SetTrue)]
        requires_response: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        encrypt: bool,
        #[arg(long, action = ArgAction::SetTrue)]
        sign: bool,
        body: String,
    },
    List {
        session: String,
        #[arg(long, action = ArgAction::SetTrue)]
        unread: bool,
        #[arg(long)]
        limit: Option<usize>,
    },
    Ack {
        session: String,
        message_id: String,
    },
    Unread {
        session: String,
    },
}

#[derive(Subcommand, Debug)]
enum TaskCmd {
    Add {
        #[arg(long)]
        workflow: String,
        title: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        created_by: String,
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
    },
    List {
        #[arg(long)]
        workflow: String,
        #[arg(long)]
        session: String,
        #[arg(long, action = ArgAction::SetTrue)]
        include_done: bool,
    },
    Claim {
        task_id: String,
        session: String,
    },
    Status {
        task_id: String,
        session: String,
        status: String,
    },
}

#[derive(Subcommand, Debug)]
enum WorkflowCmd {
    Create {
        name: String,
        #[arg(long)]
        root: String,
    },
    Join {
        workflow_id: String,
        #[arg(long)]
        session: String,
        #[arg(long)]
        parent: String,
    },
    Leave {
        workflow_id: String,
        #[arg(long)]
        session: String,
    },
    List {
        #[arg(long)]
        session: String,
    },
}

#[derive(Subcommand, Debug)]
enum NodeCmd {
    Info,
}

#[derive(Subcommand, Debug)]
enum InviteCmd {
    Create {
        #[arg(long, default_value_t = 3_600_000)]
        ttl_ms: u64,
        #[arg(long = "relay")]
        relay_hosts: Vec<String>,
        #[arg(long = "scope")]
        scopes: Vec<String>,
    },
    Accept {
        token: String,
    },
}

#[derive(Subcommand, Debug)]
enum FriendsCmd {
    List,
    Block { node_id: String },
    Unblock { node_id: String },
    Remove { node_id: String },
    Trust { node_id: String, tier: String },
}

#[derive(Subcommand, Debug)]
enum InboxCmd {
    List {
        #[arg(long, action = ArgAction::SetTrue)]
        unread: bool,
        #[arg(long)]
        limit: Option<usize>,
    },
    Ack {
        message_id: String,
    },
}

#[derive(Subcommand, Debug)]
enum RemoteCmd {
    Send {
        node_id: String,
        body: String,
        #[arg(long)]
        topic: Option<String>,
        #[arg(long, action = ArgAction::SetTrue)]
        encrypt: bool,
        #[arg(long)]
        invite_token: Option<String>,
        #[arg(long, default_value = "dm_text")]
        message_type: String,
    },
}

struct Client {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
    writer: FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
    protocol_version: u32,
    features: Vec<String>,
}

impl Client {
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
            protocol_version: 0,
            features: Vec::new(),
        };

        let first = client.recv().await?;
        match first {
            Response::Hello {
                protocol_version,
                features,
            } => {
                client.protocol_version = protocol_version;
                client.features = features;
                Ok(client)
            }
            other => bail!("expected server hello, got {other:?}"),
        }
    }

    async fn send(&mut self, req: &Request) -> Result<()> {
        let line = serde_json::to_string(req)?;
        self.writer.send(line).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Response> {
        let Some(line) = self.reader.next().await else {
            bail!("server disconnected");
        };
        let line = line?;
        let resp: Response = serde_json::from_str(&line)?;
        Ok(resp)
    }

    async fn request_ok(&mut self, req: &Request) -> Result<Option<serde_json::Value>> {
        self.send(req).await?;
        loop {
            match self.recv().await? {
                Response::Hello { .. } => continue,
                Response::Event { .. } => continue,
                Response::Ok { data } => return Ok(data),
                Response::Error { message, .. } => bail!(message),
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = resolve_socket_path(cli.socket);

    match cli.command {
        CommandGroup::Server { cmd } => match cmd {
            ServerCmd::Start { foreground } => server_start(&socket, foreground)?,
            ServerCmd::Stop => {
                let mut client = Client::connect(&socket).await?;
                let _ = client.request_ok(&Request::ServerShutdown).await?;
                println!("server stop requested");
            }
            ServerCmd::Status => {
                if UnixStream::connect(&socket).await.is_ok() {
                    println!("running ({})", socket.display());
                } else {
                    println!("not running ({})", socket.display());
                }
            }
        },
        CommandGroup::New {
            exec,
            shell,
            tags,
            worktree,
            label,
            sandbox_write,
            no_sandbox,
            parent,
            cols,
            rows,
            args,
        } => {
            let (exec, args) = resolve_command(exec, shell, args)?;
            let cwd = if let Some(branch) = worktree.as_deref() {
                let current_dir = std::env::current_dir().context("failed to read current dir")?;
                Some(tmax_git::create_worktree_for_branch(&current_dir, branch)?)
            } else {
                None
            };
            let sandbox = if no_sandbox {
                None
            } else {
                Some(SandboxConfig {
                    writable_paths: sandbox_write,
                    readable_paths: vec![],
                })
            };

            let mut client = Client::connect(&socket).await?;
            let data = client
                .request_ok(&Request::SessionCreate {
                    exec,
                    args,
                    tags,
                    cwd,
                    label,
                    sandbox,
                    parent_id: parent,
                    cols,
                    rows,
                })
                .await?;
            print_json(data.unwrap_or_else(|| json!({})))?;
        }
        CommandGroup::List { tree } => {
            let mut client = Client::connect(&socket).await?;
            let req = if tree {
                Request::SessionTree
            } else {
                Request::SessionList
            };
            let data = client.request_ok(&req).await?.unwrap_or_else(|| json!([]));
            print_json(data)?;
        }
        CommandGroup::Info { session } => {
            let mut client = Client::connect(&socket).await?;
            let data = client
                .request_ok(&Request::SessionInfo {
                    session_id: session,
                })
                .await?
                .unwrap_or_else(|| json!({}));
            print_json(data)?;
        }
        CommandGroup::Attach {
            session,
            view,
            last_seq,
        } => {
            let mut client = Client::connect(&socket).await?;
            let mode = if view {
                AttachMode::View
            } else {
                AttachMode::Edit
            };

            client
                .send(&Request::Attach {
                    session_id: session.clone(),
                    mode,
                    last_seq_seen: last_seq,
                })
                .await?;

            let attachment_id = loop {
                match client.recv().await? {
                    Response::Ok { data } => {
                        let Some(data) = data else {
                            continue;
                        };
                        if let Some(id) = data
                            .get("attachment")
                            .and_then(|a| a.get("attachment_id"))
                            .and_then(|v| v.as_str())
                        {
                            eprintln!("attachment_id={id}");
                            break id.to_string();
                        }
                    }
                    Response::Error { message, .. } => bail!(message),
                    Response::Event { event } => {
                        print_event_stdout(event.as_ref()).await?;
                    }
                    Response::Hello { .. } => {}
                }
            };

            let stdin_task = if mode == AttachMode::Edit {
                let socket_path = socket.clone();
                let session_id = session.clone();
                let attachment = attachment_id.clone();
                Some(tokio::spawn(async move {
                    if let Err(err) = pump_stdin(socket_path, session_id, attachment).await {
                        eprintln!("stdin pump stopped: {err}");
                    }
                }))
            } else {
                None
            };

            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        let _ = client.request_ok(&Request::Detach { attachment_id: attachment_id.clone() }).await;
                        break;
                    }
                    resp = client.recv() => {
                        match resp {
                            Ok(Response::Event { event }) => {
                                print_event_stdout(event.as_ref()).await?;
                            }
                            Ok(Response::Error { message, .. }) => bail!(message),
                            Ok(_) => {}
                            Err(err) => {
                                eprintln!("connection closed: {err}");
                                break;
                            }
                        }
                    }
                }
            }

            if let Some(task) = stdin_task {
                task.abort();
            }
        }
        CommandGroup::Detach { attachment } => {
            let mut client = Client::connect(&socket).await?;
            let data = client
                .request_ok(&Request::Detach {
                    attachment_id: attachment,
                })
                .await?
                .unwrap_or_else(|| json!({}));
            print_json(data)?;
        }
        CommandGroup::Send {
            session,
            input,
            attachment,
        } => {
            let mut client = Client::connect(&socket).await?;
            let payload = base64::engine::general_purpose::STANDARD.encode(input.as_bytes());
            let data = client
                .request_ok(&Request::SendInput {
                    session_id: session,
                    attachment_id: attachment,
                    data_b64: payload,
                })
                .await?
                .unwrap_or_else(|| json!({}));
            print_json(data)?;
        }
        CommandGroup::Resize {
            session,
            cols,
            rows,
        } => {
            let mut client = Client::connect(&socket).await?;
            let data = client
                .request_ok(&Request::Resize {
                    session_id: session,
                    cols,
                    rows,
                })
                .await?
                .unwrap_or_else(|| json!({}));
            print_json(data)?;
        }
        CommandGroup::Kill { session, cascade } => {
            let mut client = Client::connect(&socket).await?;
            let data = client
                .request_ok(&Request::SessionDestroy {
                    session_id: session,
                    cascade,
                })
                .await?
                .unwrap_or_else(|| json!({}));
            print_json(data)?;
        }
        CommandGroup::Marker { session, name } => {
            let mut client = Client::connect(&socket).await?;
            let data = client
                .request_ok(&Request::MarkerInsert {
                    session_id: session,
                    name,
                })
                .await?
                .unwrap_or_else(|| json!({}));
            print_json(data)?;
        }
        CommandGroup::Markers { session } => {
            let mut client = Client::connect(&socket).await?;
            let data = client
                .request_ok(&Request::MarkerList {
                    session_id: session,
                })
                .await?
                .unwrap_or_else(|| json!([]));
            print_json(data)?;
        }
        CommandGroup::Stream { session, last_seq } => {
            let mut client = Client::connect(&socket).await?;
            client
                .send(&Request::Subscribe {
                    session_id: session,
                    last_seq_seen: last_seq,
                })
                .await?;

            loop {
                match client.recv().await? {
                    Response::Event { event } => {
                        print_output_only(event.as_ref()).await?;
                    }
                    Response::Error { message, .. } => bail!(message),
                    _ => {}
                }
            }
        }
        CommandGroup::Subscribe { session, last_seq } => {
            let mut client = Client::connect(&socket).await?;
            client
                .send(&Request::Subscribe {
                    session_id: session,
                    last_seq_seen: last_seq,
                })
                .await?;
            loop {
                let resp = client.recv().await?;
                println!("{}", serde_json::to_string(&resp)?);
            }
        }
        CommandGroup::RunTask {
            exec,
            shell,
            tags,
            worktree,
            label,
            sandbox_write,
            no_sandbox,
            parent,
            cols,
            rows,
            last_seq,
            json,
            no_stream,
            timeout_ms,
            retry_attempts,
            retry_base_ms,
            retry_max_ms,
            args,
        } => {
            let (exec, args) = resolve_command(exec, shell, args)?;
            let cwd = if let Some(branch) = worktree.as_deref() {
                let current_dir = std::env::current_dir().context("failed to read current dir")?;
                Some(tmax_git::create_worktree_for_branch(&current_dir, branch)?)
            } else {
                None
            };
            let sandbox = if no_sandbox {
                None
            } else {
                Some(SandboxConfig {
                    writable_paths: sandbox_write,
                    readable_paths: vec![],
                })
            };
            let retry_policy = RetryPolicy {
                max_attempts: retry_attempts,
                base_delay: Duration::from_millis(retry_base_ms),
                max_delay: Duration::from_millis(retry_max_ms),
            };
            let run_options = RunTaskOptions {
                exec,
                args,
                tags,
                cwd,
                label,
                sandbox,
                parent_id: parent,
                cols,
                rows,
                last_seq_seen: last_seq.or(Some(0)),
            };

            let managed_execution = timeout_ms.is_some()
                || retry_attempts != 3
                || retry_base_ms != 100
                || retry_max_ms != 2000;

            let result = if managed_execution {
                let collected = execute_task_and_collect(
                    &socket,
                    run_options,
                    ExecutionOptions {
                        timeout: timeout_ms.map(Duration::from_millis),
                        retry_policy,
                        cancel_on_timeout: true,
                        cancel_cascade: true,
                    },
                )
                .await?;
                if !no_stream {
                    write_task_output(&collected.output)?;
                }
                collected.run
            } else {
                let mut client = AgentClient::connect(&socket).await?;
                if no_stream {
                    client.run_task(run_options, |_| Ok(())).await?
                } else {
                    client.run_task(run_options, write_task_output).await?
                }
            };

            if json {
                print_json(serde_json::to_value(&result)?)?;
            } else {
                eprintln!(
                    "task session_id={} exit_code={:?} signal={:?}",
                    result.session.session_id, result.exit_code, result.signal
                );
            }

            if !result.succeeded() {
                bail!(
                    "task {} failed exit_code={:?} signal={:?}",
                    result.session.session_id,
                    result.exit_code,
                    result.signal
                );
            }
        }
        CommandGroup::TailTask {
            session,
            last_seq,
            json,
            no_stream,
            retry_attempts,
            retry_base_ms,
            retry_max_ms,
        } => {
            let retry_policy = RetryPolicy {
                max_attempts: retry_attempts,
                base_delay: Duration::from_millis(retry_base_ms),
                max_delay: Duration::from_millis(retry_max_ms),
            };
            let result = if no_stream {
                tail_task_resumable(&socket, &session, last_seq, retry_policy, |_| Ok(())).await?
            } else {
                tail_task_resumable(&socket, &session, last_seq, retry_policy, write_task_output)
                    .await?
            };

            if json {
                print_json(serde_json::to_value(&result)?)?;
            } else {
                eprintln!(
                    "task session_id={} exit_code={:?} signal={:?}",
                    result.session_id, result.exit_code, result.signal
                );
            }

            if !result.succeeded() {
                bail!(
                    "task {} failed exit_code={:?} signal={:?}",
                    result.session_id,
                    result.exit_code,
                    result.signal
                );
            }
        }
        CommandGroup::CancelTask {
            session,
            cascade,
            json,
        } => {
            let mut client = AgentClient::connect(&socket).await?;
            client.cancel_task(&session, cascade).await?;

            if json {
                print_json(json!({
                    "session_id": session,
                    "cancelled": true,
                    "cascade": cascade,
                }))?;
            } else {
                println!("cancel requested session={} cascade={}", session, cascade);
            }
        }
        CommandGroup::Health { json } => {
            let report = check_server_health(&socket).await;
            if json {
                print_json(report.as_json())?;
            } else {
                report.print_human();
            }
            if !report.healthy() {
                bail!("health check failed");
            }
        }
        CommandGroup::Msg { cmd } => match cmd {
            MsgCmd::Send {
                to,
                from,
                topic,
                requires_response,
                encrypt,
                sign,
                body,
            } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::MessageSend {
                        from_session_id: from,
                        to_session_id: to,
                        topic,
                        body,
                        requires_response,
                        encrypt,
                        sign,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            MsgCmd::List {
                session,
                unread,
                limit,
            } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::MessageList {
                        session_id: session,
                        unread_only: unread,
                        limit,
                    })
                    .await?
                    .unwrap_or_else(|| json!([]));
                print_json(data)?;
            }
            MsgCmd::Ack {
                session,
                message_id,
            } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::MessageAck {
                        session_id: session,
                        message_id,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            MsgCmd::Unread { session } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::MessageUnreadCount {
                        session_id: session,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
        },
        CommandGroup::Tasks { cmd } => match cmd {
            TaskCmd::Add {
                workflow,
                title,
                description,
                created_by,
                depends_on,
            } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::TaskCreate {
                        workflow_id: workflow,
                        title,
                        description,
                        created_by,
                        depends_on,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            TaskCmd::List {
                workflow,
                session,
                include_done,
            } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::TaskList {
                        workflow_id: workflow,
                        session_id: session,
                        include_done,
                    })
                    .await?
                    .unwrap_or_else(|| json!([]));
                print_json(data)?;
            }
            TaskCmd::Claim { task_id, session } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::TaskClaim {
                        task_id,
                        session_id: session,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            TaskCmd::Status {
                task_id,
                session,
                status,
            } => {
                let status = parse_task_status(&status)?;
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::TaskSetStatus {
                        task_id,
                        session_id: session,
                        status,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
        },
        CommandGroup::Workflows { cmd } => match cmd {
            WorkflowCmd::Create { name, root } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::WorkflowCreate {
                        name,
                        root_session_id: root,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            WorkflowCmd::Join {
                workflow_id,
                session,
                parent,
            } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::WorkflowJoin {
                        workflow_id,
                        session_id: session,
                        parent_session_id: parent,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            WorkflowCmd::Leave {
                workflow_id,
                session,
            } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::WorkflowLeave {
                        workflow_id,
                        session_id: session,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            WorkflowCmd::List { session } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::WorkflowList {
                        session_id: session,
                    })
                    .await?
                    .unwrap_or_else(|| json!([]));
                print_json(data)?;
            }
        },
        CommandGroup::Node { cmd } => match cmd {
            NodeCmd::Info => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::NodeInfo)
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
        },
        CommandGroup::Invite { cmd } => match cmd {
            InviteCmd::Create {
                ttl_ms,
                relay_hosts,
                scopes,
            } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::InviteCreate {
                        relay_hosts,
                        scopes,
                        ttl_ms,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            InviteCmd::Accept { token } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::InviteAccept { token })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
        },
        CommandGroup::Friends { cmd } => match cmd {
            FriendsCmd::List => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::FriendsList)
                    .await?
                    .unwrap_or_else(|| json!([]));
                print_json(data)?;
            }
            FriendsCmd::Block { node_id } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::FriendsBlock { node_id })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            FriendsCmd::Unblock { node_id } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::FriendsUnblock { node_id })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            FriendsCmd::Remove { node_id } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::FriendsRemove { node_id })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
            FriendsCmd::Trust { node_id, tier } => {
                let trust_tier = parse_trust_tier(&tier)?;
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::FriendsSetTrust {
                        node_id,
                        trust_tier,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
        },
        CommandGroup::Inbox { cmd } => match cmd {
            InboxCmd::List { unread, limit } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::NodeInboxList {
                        unread_only: unread,
                        limit,
                    })
                    .await?
                    .unwrap_or_else(|| json!([]));
                print_json(data)?;
            }
            InboxCmd::Ack { message_id } => {
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::NodeInboxAck { message_id })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
        },
        CommandGroup::Remote { cmd } => match cmd {
            RemoteCmd::Send {
                node_id,
                body,
                topic,
                encrypt,
                invite_token,
                message_type,
            } => {
                let message_type = parse_mesh_message_type(&message_type)?;
                let mut client = Client::connect(&socket).await?;
                let data = client
                    .request_ok(&Request::NodeSendRemote {
                        to_node_id: node_id,
                        topic,
                        body,
                        encrypt,
                        invite_token,
                        message_type,
                    })
                    .await?
                    .unwrap_or_else(|| json!({}));
                print_json(data)?;
            }
        },
        CommandGroup::Up {
            node,
            foreground,
            peer_listen,
            relay_host,
        } => {
            // Check if already running
            if UnixStream::connect(&socket).await.is_ok() {
                println!("already running ({})", socket.display());
                return Ok(());
            }

            if node {
                let mut extra_args = vec![
                    "--peer-listen".to_string(),
                    peer_listen,
                ];
                for host in &relay_host {
                    extra_args.push("--relay-host".to_string());
                    extra_args.push(host.clone());
                }
                spawn_server("tmax-node", &socket, &extra_args, foreground)?;
            } else {
                spawn_server("tmax-local", &socket, &[], foreground)?;
            }

            // In background mode, wait for socket to become connectable
            if !foreground {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    if UnixStream::connect(&socket).await.is_ok() {
                        println!("ready");
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        eprintln!("warning: server started but socket not yet connectable");
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        CommandGroup::Down => {
            if UnixStream::connect(&socket).await.is_err() {
                println!("not running ({})", socket.display());
                return Ok(());
            }

            let mut client = Client::connect(&socket).await?;
            let _ = client.request_ok(&Request::ServerShutdown).await?;

            // Wait for socket to disappear
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if !socket.exists() {
                    println!("stopped ({})", socket.display());
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    println!("shutdown requested ({})", socket.display());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        CommandGroup::Worktree { cmd } => match cmd {
            WorktreeCmd::Clean { session } => {
                let mut client = Client::connect(&socket).await?;
                let info = client
                    .request_ok(&Request::SessionInfo {
                        session_id: session.clone(),
                    })
                    .await?
                    .ok_or_else(|| anyhow!("missing session info"))?;

                let repo_root = info
                    .get("git_repo_root")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("session has no git_repo_root"))?;
                let worktree_path = info
                    .get("git_worktree_path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("session has no git_worktree_path"))?;

                let repo_root = PathBuf::from(repo_root);
                let worktree_path = PathBuf::from(worktree_path);
                if repo_root == worktree_path || !is_managed_worktree(&worktree_path) {
                    bail!("refusing to clean unmanaged/non-worktree path");
                }

                let _ = client
                    .request_ok(&Request::SessionDestroy {
                        session_id: session.clone(),
                        cascade: true,
                    })
                    .await?;

                tmax_git::clean_worktree(&repo_root, &worktree_path)?;
                print_json(json!({
                    "session_id": session,
                    "worktree_cleaned": worktree_path,
                }))?;
            }
        },
    }

    Ok(())
}

fn resolve_socket_path(override_path: Option<PathBuf>) -> PathBuf {
    if let Some(path) = override_path {
        return path;
    }

    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("tmax").join("tmax.sock");
    }

    let uid = Uid::effective().as_raw();
    PathBuf::from(format!("/tmp/tmax-{uid}/tmax.sock"))
}

/// Resolve the executable and arguments for a session/task.
///
/// - If `--exec` is given, use it directly with the provided args.
/// - Otherwise, default to `$SHELL` (or `/bin/sh`).
///   - If args are provided and don't start with `-`, wrap them with `-c`
///     so `tmax run-task 'npm test'` just works.
///   - If args start with `-` (e.g. `-lc 'cmd'`), pass them through as-is.
fn resolve_command(exec: Option<String>, shell: bool, args: Vec<String>) -> Result<(String, Vec<String>)> {
    if let Some(exec) = exec {
        return Ok((exec, args));
    }

    let sh = if shell {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    } else {
        // Default to shell — no more requiring --exec or --shell
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    };

    if args.is_empty() {
        return Ok((sh, args));
    }

    // If args already look like shell flags (e.g. -lc 'cmd'), pass through
    if args[0].starts_with('-') {
        return Ok((sh, args));
    }

    // Otherwise, wrap args as a single -c command string
    let command = args.join(" ");
    Ok((sh, vec!["-c".to_string(), command]))
}

fn is_managed_worktree(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == ".tmax-worktrees")
}

fn parse_task_status(raw: &str) -> Result<SharedTaskStatus> {
    match raw {
        "todo" => Ok(SharedTaskStatus::Todo),
        "in_progress" | "in-progress" => Ok(SharedTaskStatus::InProgress),
        "blocked" => Ok(SharedTaskStatus::Blocked),
        "done" => Ok(SharedTaskStatus::Done),
        _ => {
            bail!("invalid task status '{raw}' (expected one of: todo, in_progress, blocked, done)")
        }
    }
}

fn parse_trust_tier(raw: &str) -> Result<TrustTier> {
    match raw {
        "public" => Ok(TrustTier::Public),
        "follower" => Ok(TrustTier::Follower),
        "trusted" => Ok(TrustTier::Trusted),
        "operator" => Ok(TrustTier::Operator),
        _ => bail!(
            "invalid trust tier '{raw}' (expected one of: public, follower, trusted, operator)"
        ),
    }
}

fn parse_mesh_message_type(raw: &str) -> Result<MeshMessageType> {
    match raw {
        "unspecified" => Ok(MeshMessageType::Unspecified),
        "dm_text" => Ok(MeshMessageType::DmText),
        "broadcast" => Ok(MeshMessageType::Broadcast),
        "task_update" => Ok(MeshMessageType::TaskUpdate),
        "command" => Ok(MeshMessageType::Command),
        _ => bail!(
            "invalid message type '{raw}' (expected one of: unspecified, dm_text, broadcast, task_update, command)"
        ),
    }
}

fn print_json(value: serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn server_start(socket: &Path, foreground: bool) -> Result<()> {
    spawn_server("tmax-local", socket, &[], foreground)
}

fn spawn_server(binary: &str, socket: &Path, extra_args: &[String], foreground: bool) -> Result<()> {
    let mut cmd = Command::new(binary);
    cmd.arg("--socket").arg(socket);
    for arg in extra_args {
        cmd.arg(arg);
    }

    if foreground {
        let status = cmd.status().with_context(|| format!("failed to start {binary}"))?;
        if !status.success() {
            bail!("{binary} exited with status {status}");
        }
        return Ok(());
    }

    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn {binary}"))?;

    println!(
        "started {binary} pid={} socket={}",
        child.id(),
        socket.display()
    );
    Ok(())
}

async fn print_event_stdout(event: &tmax_protocol::Event) -> Result<()> {
    match event {
        tmax_protocol::Event::Output { data_b64, .. } => {
            print_output_bytes(data_b64).await?;
        }
        _ => {
            eprintln!("{}", serde_json::to_string(event)?);
        }
    }
    Ok(())
}

async fn print_output_only(event: &tmax_protocol::Event) -> Result<()> {
    if let tmax_protocol::Event::Output { data_b64, .. } = event {
        print_output_bytes(data_b64).await?;
    }
    Ok(())
}

async fn print_output_bytes(data_b64: &str) -> Result<()> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&bytes).await?;
    stdout.flush().await?;
    Ok(())
}

fn write_task_output(chunk: &[u8]) -> std::result::Result<(), AgentSdkError> {
    let mut stdout = std::io::stdout();
    stdout.write_all(chunk).map_err(|err| AgentSdkError::Io {
        message: format!("failed to write task output to stdout: {err}"),
    })?;
    stdout.flush().map_err(|err| AgentSdkError::Io {
        message: format!("failed to flush task output: {err}"),
    })?;
    Ok(())
}

async fn pump_stdin(socket: PathBuf, session: String, attachment: String) -> Result<()> {
    let mut client = Client::connect(&socket).await?;
    let mut stdin = tokio::io::stdin();
    let mut buf = vec![0u8; 1024];

    loop {
        let n = stdin.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        let chunk = &buf[..n];
        let payload = base64::engine::general_purpose::STANDARD.encode(chunk);
        let _ = client
            .request_ok(&Request::SendInput {
                session_id: session.clone(),
                attachment_id: attachment.clone(),
                data_b64: payload,
            })
            .await?;
    }

    Ok(())
}

struct ServerHealthReport {
    socket: PathBuf,
    socket_exists: bool,
    connected: bool,
    protocol_version: Option<u32>,
    expected_protocol_version: u32,
    features: Vec<String>,
    session_count: Option<usize>,
    latency_ms: Option<u128>,
    errors: Vec<String>,
}

impl ServerHealthReport {
    fn healthy(&self) -> bool {
        self.errors.is_empty() && self.connected
    }

    fn as_json(&self) -> serde_json::Value {
        json!({
            "healthy": self.healthy(),
            "socket": self.socket,
            "socket_exists": self.socket_exists,
            "connected": self.connected,
            "protocol_version": self.protocol_version,
            "expected_protocol_version": self.expected_protocol_version,
            "features": self.features,
            "session_count": self.session_count,
            "latency_ms": self.latency_ms,
            "errors": self.errors,
        })
    }

    fn print_human(&self) {
        if self.healthy() {
            println!(
                "healthy socket={} protocol={} sessions={} latency_ms={}",
                self.socket.display(),
                self.protocol_version.unwrap_or_default(),
                self.session_count.unwrap_or_default(),
                self.latency_ms.unwrap_or_default()
            );
            return;
        }

        eprintln!(
            "unhealthy socket={} connected={} socket_exists={}",
            self.socket.display(),
            self.connected,
            self.socket_exists
        );
        for err in &self.errors {
            eprintln!("error: {err}");
        }
    }
}

async fn check_server_health(socket: &Path) -> ServerHealthReport {
    let started = std::time::Instant::now();
    let socket_exists = socket.exists();
    let mut errors = Vec::new();
    let mut connected = false;
    let mut protocol_version = None;
    let mut features = Vec::new();
    let mut session_count = None;

    if !socket_exists {
        errors.push(format!("socket does not exist: {}", socket.display()));
    }

    match Client::connect(socket).await {
        Ok(mut client) => {
            connected = true;
            protocol_version = Some(client.protocol_version);
            features = client.features.clone();

            if client.protocol_version != tmax_protocol::PROTOCOL_VERSION {
                errors.push(format!(
                    "protocol mismatch: server={} expected={}",
                    client.protocol_version,
                    tmax_protocol::PROTOCOL_VERSION
                ));
            }

            match client.request_ok(&Request::SessionList).await {
                Ok(Some(data)) => {
                    session_count = data.as_array().map(std::vec::Vec::len);
                    if session_count.is_none() {
                        errors.push("session_list returned non-array payload".to_string());
                    }
                }
                Ok(None) => {
                    errors.push("session_list returned empty payload".to_string());
                }
                Err(err) => {
                    errors.push(format!("session_list request failed: {err}"));
                }
            }
        }
        Err(err) => {
            errors.push(err.to_string());
        }
    }

    ServerHealthReport {
        socket: socket.to_path_buf(),
        socket_exists,
        connected,
        protocol_version,
        expected_protocol_version: tmax_protocol::PROTOCOL_VERSION,
        features,
        session_count,
        latency_ms: Some(started.elapsed().as_millis()),
        errors,
    }
}

#[allow(dead_code)]
fn _as_summary(value: serde_json::Value) -> Result<SessionSummary> {
    let parsed: SessionSummary = serde_json::from_value(value)?;
    Ok(parsed)
}
