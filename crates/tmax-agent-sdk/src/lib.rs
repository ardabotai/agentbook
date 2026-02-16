use anyhow::anyhow;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tmax_protocol::{
    AgentMessage, AgentWalletPublic, MAX_JSON_LINE_BYTES, OrchestrationWorkflow, PROTOCOL_VERSION,
    Request, Response, SandboxConfig, SessionSummary, SharedTask, SharedTaskStatus,
};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

const DEFAULT_DEPLOY_SCRIPT: &str = "scripts/deploy-linux.sh";
const DEFAULT_ROLLBACK_SCRIPT: &str = "scripts/rollback-linux.sh";

pub type AgentResult<T> = std::result::Result<T, AgentSdkError>;

#[derive(Debug, Error)]
pub enum AgentSdkError {
    #[error("connection error: {message}")]
    Connection { message: String },
    #[error("protocol error: {message}")]
    Protocol { message: String },
    #[error("server error: {message}")]
    Server { message: String },
    #[error("timeout while {operation} after {timeout_ms}ms (session_id={session_id:?})")]
    Timeout {
        operation: String,
        timeout_ms: u64,
        session_id: Option<String>,
    },
    #[error("task failed: session_id={session_id} exit_code={exit_code:?} signal={signal:?}")]
    TaskFailed {
        session_id: String,
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    #[error("retry exhausted for {operation} after {attempts} attempts: {last_error}")]
    RetryExhausted {
        operation: String,
        attempts: u32,
        last_error: String,
    },
    #[error("command failed ({command}) status={status:?}: {stderr}")]
    CommandFailed {
        command: String,
        status: Option<i32>,
        stderr: String,
    },
    #[error("io error: {message}")]
    Io { message: String },
}

impl AgentSdkError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Connection { .. } | Self::Io { .. } | Self::Timeout { .. }
        )
    }
}

fn classify_anyhow(err: anyhow::Error) -> AgentSdkError {
    let msg = err.to_string();
    if msg.contains("failed to connect")
        || msg.contains("server disconnected")
        || msg.contains("socket write failed")
        || msg.contains("socket read failed")
    {
        return AgentSdkError::Connection { message: msg };
    }
    if msg.contains("invalid json")
        || msg.contains("expected server hello")
        || msg.contains("decode")
    {
        return AgentSdkError::Protocol { message: msg };
    }
    AgentSdkError::Io { message: msg }
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    fn normalized_attempts(&self) -> u32 {
        self.max_attempts.max(1)
    }

    fn backoff_delay(&self, attempt: u32) -> Duration {
        let capped = attempt.min(31);
        let mult = 1u64 << capped.saturating_sub(1);
        let millis = self.base_delay.as_millis() as u64;
        let raw = millis.saturating_mul(mult);
        Duration::from_millis(raw.min(self.max_delay.as_millis() as u64))
    }
}

pub struct AgentClient {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
    writer: FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
    protocol_version: u32,
    features: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RunTaskOptions {
    pub exec: String,
    pub args: Vec<String>,
    pub tags: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub label: Option<String>,
    pub sandbox: Option<SandboxConfig>,
    pub parent_id: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub last_seq_seen: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ExecutionOptions {
    pub timeout: Option<Duration>,
    pub retry_policy: RetryPolicy,
    pub cancel_on_timeout: bool,
    pub cancel_cascade: bool,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            retry_policy: RetryPolicy::default(),
            cancel_on_timeout: true,
            cancel_cascade: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTaskResult {
    pub session: SessionSummary,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub last_seq: Option<u64>,
}

impl RunTaskResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && self.signal.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailTaskResult {
    pub session_id: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub last_seq: Option<u64>,
}

impl TailTaskResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && self.signal.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedTaskResult {
    pub run: RunTaskResult,
    pub output: Vec<u8>,
}

impl CollectedTaskResult {
    pub fn output_utf8_lossy(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub socket: PathBuf,
    pub socket_exists: bool,
    pub connected: bool,
    pub protocol_version: Option<u32>,
    pub expected_protocol_version: u32,
    pub features: Vec<String>,
    pub session_count: Option<usize>,
    pub latency_ms: u128,
    pub errors: Vec<String>,
}

impl HealthReport {
    pub fn healthy(&self) -> bool {
        self.connected && self.errors.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub command: String,
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct DeployOptions {
    pub script_path: PathBuf,
    pub artifact: PathBuf,
    pub release_name: Option<String>,
    pub install_root: Option<PathBuf>,
    pub etc_dir: Option<PathBuf>,
    pub service_name: Option<String>,
    pub socket: Option<PathBuf>,
    pub dry_run: bool,
}

impl DeployOptions {
    pub fn new(artifact: impl Into<PathBuf>) -> Self {
        Self {
            script_path: PathBuf::from(DEFAULT_DEPLOY_SCRIPT),
            artifact: artifact.into(),
            release_name: None,
            install_root: None,
            etc_dir: None,
            service_name: None,
            socket: None,
            dry_run: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RollbackOptions {
    pub script_path: PathBuf,
    pub target: Option<String>,
    pub install_root: Option<PathBuf>,
    pub service_name: Option<String>,
    pub socket: Option<PathBuf>,
    pub dry_run: bool,
}

impl Default for RollbackOptions {
    fn default() -> Self {
        Self {
            script_path: PathBuf::from(DEFAULT_ROLLBACK_SCRIPT),
            target: None,
            install_root: None,
            service_name: None,
            socket: None,
            dry_run: false,
        }
    }
}

impl AgentClient {
    pub async fn connect(socket_path: &Path) -> AgentResult<Self> {
        let stream =
            UnixStream::connect(socket_path)
                .await
                .map_err(|err| AgentSdkError::Connection {
                    message: format!("failed to connect {}: {err}", socket_path.display()),
                })?;
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

        match client.recv().await? {
            Response::Hello {
                protocol_version,
                features,
            } => {
                client.protocol_version = protocol_version;
                client.features = features;
                Ok(client)
            }
            other => Err(AgentSdkError::Protocol {
                message: format!("expected server hello, got {other:?}"),
            }),
        }
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn protocol_compatible(&self) -> bool {
        self.protocol_version == PROTOCOL_VERSION
    }

    pub fn features(&self) -> &[String] {
        &self.features
    }

    pub async fn create_task(&mut self, options: &RunTaskOptions) -> AgentResult<SessionSummary> {
        let data = self
            .request_ok(&Request::SessionCreate {
                exec: options.exec.clone(),
                args: options.args.clone(),
                tags: options.tags.clone(),
                cwd: options.cwd.clone(),
                label: options.label.clone(),
                sandbox: options.sandbox.clone(),
                parent_id: options.parent_id.clone(),
                cols: options.cols,
                rows: options.rows,
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing session create payload".to_string(),
            })?;

        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse session summary from create response: {err}"),
        })
    }

    pub async fn run_task<F>(
        &mut self,
        options: RunTaskOptions,
        mut on_output: F,
    ) -> AgentResult<RunTaskResult>
    where
        F: FnMut(&[u8]) -> AgentResult<()>,
    {
        let session = self.create_task(&options).await?;

        let tail = self
            .tail_task(
                &session.session_id,
                options.last_seq_seen.or(Some(0)),
                |chunk| on_output(chunk),
            )
            .await?;

        Ok(RunTaskResult {
            session,
            exit_code: tail.exit_code,
            signal: tail.signal,
            last_seq: tail.last_seq,
        })
    }

    pub async fn tail_task_with_seq<F>(
        &mut self,
        session_id: &str,
        last_seq_seen: Option<u64>,
        mut on_output: F,
    ) -> AgentResult<TailTaskResult>
    where
        F: FnMut(u64, &[u8]) -> AgentResult<()>,
    {
        self.send(&Request::Subscribe {
            session_id: session_id.to_string(),
            last_seq_seen,
        })
        .await?;

        let mut last_seq = last_seq_seen;

        loop {
            match self.recv().await? {
                Response::Hello { .. } | Response::Ok { .. } => continue,
                Response::Error { message, .. } => {
                    return Err(AgentSdkError::Server { message });
                }
                Response::Event { event } => match *event {
                    tmax_protocol::Event::Output {
                        session_id: event_session,
                        seq,
                        data_b64,
                    } => {
                        if event_session != session_id {
                            continue;
                        }
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(data_b64)
                            .map_err(|err| AgentSdkError::Protocol {
                                message: format!("failed to decode output chunk: {err}"),
                            })?;
                        on_output(seq, &bytes)?;
                        last_seq = Some(seq);
                    }
                    tmax_protocol::Event::Snapshot {
                        session_id: event_session,
                        seq,
                        lines,
                        ..
                    } => {
                        if event_session != session_id {
                            continue;
                        }
                        let mut rendered = lines.join("\n").into_bytes();
                        if !rendered.is_empty() {
                            rendered.push(b'\n');
                            on_output(seq, &rendered)?;
                        }
                        last_seq = Some(seq);
                    }
                    tmax_protocol::Event::SessionExited {
                        session_id: event_session,
                        exit_code,
                        signal,
                    } => {
                        if event_session != session_id {
                            continue;
                        }
                        return Ok(TailTaskResult {
                            session_id: event_session,
                            exit_code,
                            signal,
                            last_seq,
                        });
                    }
                    _ => {}
                },
            }
        }
    }

    pub async fn tail_task<F>(
        &mut self,
        session_id: &str,
        last_seq_seen: Option<u64>,
        mut on_output: F,
    ) -> AgentResult<TailTaskResult>
    where
        F: FnMut(&[u8]) -> AgentResult<()>,
    {
        self.tail_task_with_seq(session_id, last_seq_seen, |_, chunk| on_output(chunk))
            .await
    }

    pub async fn cancel_task(&mut self, session_id: &str, cascade: bool) -> AgentResult<()> {
        let _ = self
            .request_ok(&Request::SessionDestroy {
                session_id: session_id.to_string(),
                cascade,
            })
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_message(
        &mut self,
        from_session_id: Option<String>,
        to_session_id: String,
        topic: Option<String>,
        body: String,
        requires_response: bool,
        encrypt: bool,
        sign: bool,
    ) -> AgentResult<AgentMessage> {
        let data = self
            .request_ok(&Request::MessageSend {
                from_session_id,
                to_session_id,
                topic,
                body,
                requires_response,
                encrypt,
                sign,
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing message_send payload".to_string(),
            })?;
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse agent message: {err}"),
        })
    }

    pub async fn list_messages(
        &mut self,
        session_id: &str,
        unread_only: bool,
        limit: Option<usize>,
    ) -> AgentResult<Vec<AgentMessage>> {
        let data = self
            .request_ok(&Request::MessageList {
                session_id: session_id.to_string(),
                unread_only,
                limit,
            })
            .await?
            .unwrap_or_else(|| serde_json::json!([]));
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse messages list: {err}"),
        })
    }

    pub async fn ack_message(
        &mut self,
        session_id: &str,
        message_id: &str,
    ) -> AgentResult<AgentMessage> {
        let data = self
            .request_ok(&Request::MessageAck {
                session_id: session_id.to_string(),
                message_id: message_id.to_string(),
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing message_ack payload".to_string(),
            })?;
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse acked message: {err}"),
        })
    }

    pub async fn unread_message_count(&mut self, session_id: &str) -> AgentResult<usize> {
        let data = self
            .request_ok(&Request::MessageUnreadCount {
                session_id: session_id.to_string(),
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing message unread count payload".to_string(),
            })?;
        data.get("unread")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "message unread count payload missing 'unread'".to_string(),
            })
    }

    pub async fn wallet_info(&mut self, session_id: &str) -> AgentResult<AgentWalletPublic> {
        let data = self
            .request_ok(&Request::WalletInfo {
                session_id: session_id.to_string(),
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing wallet_info payload".to_string(),
            })?;
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse wallet info: {err}"),
        })
    }

    pub async fn create_shared_task(
        &mut self,
        workflow_id: String,
        title: String,
        description: Option<String>,
        created_by: String,
        depends_on: Vec<String>,
    ) -> AgentResult<SharedTask> {
        let data = self
            .request_ok(&Request::TaskCreate {
                workflow_id,
                title,
                description,
                created_by,
                depends_on,
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing task_create payload".to_string(),
            })?;
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse shared task: {err}"),
        })
    }

    pub async fn list_shared_tasks(
        &mut self,
        workflow_id: String,
        session_id: String,
        include_done: bool,
    ) -> AgentResult<Vec<SharedTask>> {
        let data = self
            .request_ok(&Request::TaskList {
                workflow_id,
                session_id,
                include_done,
            })
            .await?
            .unwrap_or_else(|| serde_json::json!([]));
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse shared task list: {err}"),
        })
    }

    pub async fn claim_shared_task(
        &mut self,
        task_id: &str,
        session_id: &str,
    ) -> AgentResult<SharedTask> {
        let data = self
            .request_ok(&Request::TaskClaim {
                task_id: task_id.to_string(),
                session_id: session_id.to_string(),
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing task_claim payload".to_string(),
            })?;
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse claimed task: {err}"),
        })
    }

    pub async fn set_shared_task_status(
        &mut self,
        task_id: &str,
        session_id: &str,
        status: SharedTaskStatus,
    ) -> AgentResult<SharedTask> {
        let data = self
            .request_ok(&Request::TaskSetStatus {
                task_id: task_id.to_string(),
                session_id: session_id.to_string(),
                status,
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing task_set_status payload".to_string(),
            })?;
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse updated task: {err}"),
        })
    }

    pub async fn create_workflow(
        &mut self,
        name: String,
        root_session_id: String,
    ) -> AgentResult<OrchestrationWorkflow> {
        let data = self
            .request_ok(&Request::WorkflowCreate {
                name,
                root_session_id,
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing workflow_create payload".to_string(),
            })?;
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse workflow: {err}"),
        })
    }

    pub async fn join_workflow(
        &mut self,
        workflow_id: String,
        session_id: String,
        parent_session_id: String,
    ) -> AgentResult<OrchestrationWorkflow> {
        let data = self
            .request_ok(&Request::WorkflowJoin {
                workflow_id,
                session_id,
                parent_session_id,
            })
            .await?
            .ok_or_else(|| AgentSdkError::Protocol {
                message: "missing workflow_join payload".to_string(),
            })?;
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse workflow: {err}"),
        })
    }

    pub async fn list_workflows(
        &mut self,
        session_id: String,
    ) -> AgentResult<Vec<OrchestrationWorkflow>> {
        let data = self
            .request_ok(&Request::WorkflowList { session_id })
            .await?
            .unwrap_or_else(|| serde_json::json!([]));
        serde_json::from_value(data).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to parse workflow list: {err}"),
        })
    }

    async fn send(&mut self, req: &Request) -> AgentResult<()> {
        let line = serde_json::to_string(req).map_err(|err| AgentSdkError::Protocol {
            message: format!("failed to encode request: {err}"),
        })?;
        self.writer
            .send(line)
            .await
            .map_err(|err| AgentSdkError::Connection {
                message: format!("socket write failed: {err}"),
            })?;
        Ok(())
    }

    async fn recv(&mut self) -> AgentResult<Response> {
        let Some(line) = self.reader.next().await else {
            return Err(AgentSdkError::Connection {
                message: "server disconnected".to_string(),
            });
        };
        let line = line.map_err(|err| AgentSdkError::Connection {
            message: format!("socket read failed: {err}"),
        })?;
        let resp: Response =
            serde_json::from_str(&line).map_err(|err| AgentSdkError::Protocol {
                message: format!("invalid json response: {err}"),
            })?;
        Ok(resp)
    }

    async fn request_ok(&mut self, req: &Request) -> AgentResult<Option<serde_json::Value>> {
        self.send(req).await?;
        loop {
            match self.recv().await? {
                Response::Hello { .. } => continue,
                Response::Event { .. } => continue,
                Response::Ok { data } => return Ok(data),
                Response::Error { message, .. } => {
                    return Err(AgentSdkError::Server { message });
                }
            }
        }
    }
}

pub async fn cancel_task_on_socket(
    socket: &Path,
    session_id: &str,
    cascade: bool,
) -> AgentResult<()> {
    let mut client = AgentClient::connect(socket).await?;
    client.cancel_task(session_id, cascade).await
}

pub async fn tail_task_resumable<F>(
    socket: &Path,
    session_id: &str,
    last_seq_seen: Option<u64>,
    retry_policy: RetryPolicy,
    mut on_output: F,
) -> AgentResult<TailTaskResult>
where
    F: FnMut(&[u8]) -> AgentResult<()>,
{
    let attempts = retry_policy.normalized_attempts();
    let mut attempt: u32 = 0;
    let mut resume_seq = last_seq_seen;
    let mut last_error = String::new();

    while attempt < attempts {
        attempt += 1;

        let mut client = match AgentClient::connect(socket).await {
            Ok(client) => client,
            Err(err) => {
                last_error = err.to_string();
                if err.is_transient() && attempt < attempts {
                    tokio::time::sleep(retry_policy.backoff_delay(attempt)).await;
                    continue;
                }
                return if err.is_transient() {
                    Err(AgentSdkError::RetryExhausted {
                        operation: "tail_task_resumable(connect)".to_string(),
                        attempts,
                        last_error,
                    })
                } else {
                    Err(err)
                };
            }
        };

        let run = client
            .tail_task_with_seq(session_id, resume_seq, |seq, chunk| {
                on_output(chunk)?;
                resume_seq = Some(seq);
                Ok(())
            })
            .await;

        match run {
            Ok(done) => return Ok(done),
            Err(err) => {
                last_error = err.to_string();
                if err.is_transient() && attempt < attempts {
                    tokio::time::sleep(retry_policy.backoff_delay(attempt)).await;
                    continue;
                }
                return if err.is_transient() {
                    Err(AgentSdkError::RetryExhausted {
                        operation: format!("tail_task_resumable({session_id})"),
                        attempts,
                        last_error,
                    })
                } else {
                    Err(err)
                };
            }
        }
    }

    Err(AgentSdkError::RetryExhausted {
        operation: format!("tail_task_resumable({session_id})"),
        attempts,
        last_error,
    })
}

pub async fn execute_task_and_collect(
    socket: &Path,
    options: RunTaskOptions,
    execution: ExecutionOptions,
) -> AgentResult<CollectedTaskResult> {
    let attempts = execution.retry_policy.normalized_attempts();
    let mut attempt: u32 = 0;

    let session = loop {
        attempt += 1;

        let mut client = match AgentClient::connect(socket).await {
            Ok(client) => client,
            Err(err) => {
                let last_error = err.to_string();
                if err.is_transient() && attempt < attempts {
                    tokio::time::sleep(execution.retry_policy.backoff_delay(attempt)).await;
                    continue;
                }
                return if err.is_transient() {
                    Err(AgentSdkError::RetryExhausted {
                        operation: "execute_task_and_collect(connect/create)".to_string(),
                        attempts,
                        last_error,
                    })
                } else {
                    Err(err)
                };
            }
        };

        match client.create_task(&options).await {
            Ok(session) => break session,
            Err(err) => {
                let last_error = err.to_string();
                if err.is_transient() && attempt < attempts {
                    tokio::time::sleep(execution.retry_policy.backoff_delay(attempt)).await;
                    continue;
                }
                return if err.is_transient() {
                    Err(AgentSdkError::RetryExhausted {
                        operation: "execute_task_and_collect(connect/create)".to_string(),
                        attempts,
                        last_error,
                    })
                } else {
                    Err(err)
                };
            }
        }
    };

    let mut output = Vec::new();
    let session_id = session.session_id.clone();
    let tail_future = tail_task_resumable(
        socket,
        &session_id,
        options.last_seq_seen.or(Some(0)),
        execution.retry_policy.clone(),
        |chunk| {
            output.extend_from_slice(chunk);
            Ok(())
        },
    );

    let tail = if let Some(timeout) = execution.timeout {
        match tokio::time::timeout(timeout, tail_future).await {
            Ok(result) => result?,
            Err(_) => {
                if execution.cancel_on_timeout {
                    let _ =
                        cancel_task_on_socket(socket, &session_id, execution.cancel_cascade).await;
                }
                return Err(AgentSdkError::Timeout {
                    operation: "execute_task_and_collect".to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                    session_id: Some(session_id),
                });
            }
        }
    } else {
        tail_future.await?
    };

    let run = RunTaskResult {
        session,
        exit_code: tail.exit_code,
        signal: tail.signal,
        last_seq: tail.last_seq,
    };

    if !run.succeeded() {
        return Err(AgentSdkError::TaskFailed {
            session_id: run.session.session_id.clone(),
            exit_code: run.exit_code,
            signal: run.signal,
        });
    }

    Ok(CollectedTaskResult { run, output })
}

pub async fn health(socket: &Path) -> HealthReport {
    let started = tokio::time::Instant::now();
    let socket_exists = socket.exists();
    let mut report = HealthReport {
        socket: socket.to_path_buf(),
        socket_exists,
        connected: false,
        protocol_version: None,
        expected_protocol_version: PROTOCOL_VERSION,
        features: Vec::new(),
        session_count: None,
        latency_ms: 0,
        errors: Vec::new(),
    };

    if !socket_exists {
        report
            .errors
            .push(format!("socket does not exist: {}", socket.display()));
        report.latency_ms = started.elapsed().as_millis();
        return report;
    }

    match AgentClient::connect(socket).await {
        Ok(mut client) => {
            report.connected = true;
            report.protocol_version = Some(client.protocol_version());
            report.features = client.features().to_vec();

            if !client.protocol_compatible() {
                report.errors.push(format!(
                    "protocol mismatch: server={} expected={}",
                    client.protocol_version(),
                    PROTOCOL_VERSION
                ));
            }

            match client.request_ok(&Request::SessionList).await {
                Ok(Some(data)) => {
                    report.session_count = data.as_array().map(std::vec::Vec::len);
                    if report.session_count.is_none() {
                        report
                            .errors
                            .push("session_list returned non-array payload".to_string());
                    }
                }
                Ok(None) => {
                    report
                        .errors
                        .push("session_list returned empty payload".to_string());
                }
                Err(err) => {
                    report
                        .errors
                        .push(format!("session_list request failed: {err}"));
                }
            }
        }
        Err(err) => {
            report.errors.push(err.to_string());
        }
    }

    report.latency_ms = started.elapsed().as_millis();
    report
}

pub async fn wait_ready(
    socket: &Path,
    timeout: Duration,
    poll_interval: Duration,
) -> AgentResult<HealthReport> {
    let started = tokio::time::Instant::now();
    let interval = if poll_interval.is_zero() {
        Duration::from_millis(100)
    } else {
        poll_interval
    };

    loop {
        let report = health(socket).await;
        if report.healthy() {
            return Ok(report);
        }

        if started.elapsed() >= timeout {
            return Err(AgentSdkError::Timeout {
                operation: "wait_ready".to_string(),
                timeout_ms: timeout.as_millis() as u64,
                session_id: None,
            });
        }

        tokio::time::sleep(interval).await;
    }
}

pub async fn run_deploy(options: DeployOptions) -> AgentResult<CommandOutcome> {
    let mut cmd = Command::new(&options.script_path);
    cmd.arg("--artifact").arg(&options.artifact);

    if let Some(release_name) = &options.release_name {
        cmd.arg("--release-name").arg(release_name);
    }
    if let Some(install_root) = &options.install_root {
        cmd.arg("--install-root").arg(install_root);
    }
    if let Some(etc_dir) = &options.etc_dir {
        cmd.arg("--etc-dir").arg(etc_dir);
    }
    if let Some(service_name) = &options.service_name {
        cmd.arg("--service-name").arg(service_name);
    }
    if let Some(socket) = &options.socket {
        cmd.arg("--socket").arg(socket);
    }
    if options.dry_run {
        cmd.arg("--dry-run");
    }

    run_script_command(cmd, "deploy-linux").await
}

pub async fn run_rollback(options: RollbackOptions) -> AgentResult<CommandOutcome> {
    let mut cmd = Command::new(&options.script_path);

    if let Some(target) = &options.target {
        cmd.arg("--target").arg(target);
    }
    if let Some(install_root) = &options.install_root {
        cmd.arg("--install-root").arg(install_root);
    }
    if let Some(service_name) = &options.service_name {
        cmd.arg("--service-name").arg(service_name);
    }
    if let Some(socket) = &options.socket {
        cmd.arg("--socket").arg(socket);
    }
    if options.dry_run {
        cmd.arg("--dry-run");
    }

    run_script_command(cmd, "rollback-linux").await
}

async fn run_script_command(mut cmd: Command, label: &str) -> AgentResult<CommandOutcome> {
    let display = format!("{} {:?}", label, cmd.as_std());
    let output = cmd.output().await.map_err(|err| AgentSdkError::Io {
        message: format!("failed to run {label}: {err}"),
    })?;

    let stdout = String::from_utf8(output.stdout).map_err(|err| AgentSdkError::Protocol {
        message: format!("{label} stdout is not utf8: {err}"),
    })?;
    let stderr = String::from_utf8(output.stderr).map_err(|err| AgentSdkError::Protocol {
        message: format!("{label} stderr is not utf8: {err}"),
    })?;
    let status = output.status.code().unwrap_or(-1);

    if !output.status.success() {
        return Err(AgentSdkError::CommandFailed {
            command: display,
            status: output.status.code(),
            stderr,
        });
    }

    Ok(CommandOutcome {
        command: display,
        status,
        stdout,
        stderr,
    })
}

pub async fn retry_with_policy<F, Fut, T>(
    operation: &str,
    policy: RetryPolicy,
    mut action: F,
) -> AgentResult<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = AgentResult<T>>,
{
    let attempts = policy.normalized_attempts();
    let mut attempt = 0;
    let mut last_error = String::new();

    while attempt < attempts {
        attempt += 1;
        match action(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                last_error = err.to_string();
                if err.is_transient() && attempt < attempts {
                    tokio::time::sleep(policy.backoff_delay(attempt)).await;
                    continue;
                }
                return if err.is_transient() {
                    Err(AgentSdkError::RetryExhausted {
                        operation: operation.to_string(),
                        attempts,
                        last_error,
                    })
                } else {
                    Err(err)
                };
            }
        }
    }

    Err(AgentSdkError::RetryExhausted {
        operation: operation.to_string(),
        attempts,
        last_error,
    })
}

pub fn map_anyhow(err: anyhow::Error) -> AgentSdkError {
    classify_anyhow(err)
}

pub fn context_error(context: &str, err: anyhow::Error) -> AgentSdkError {
    let wrapped = anyhow!(err).context(context.to_string());
    classify_anyhow(wrapped)
}
