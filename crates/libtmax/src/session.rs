use crate::broker::EventBroker;
use crate::output::{HistoryLog, LiveBuffer, OutputChunk};
use crate::vt_state::VtState;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use k256::ecdh::diffie_hellman;
use k256::ecdsa::SigningKey;
use k256::{PublicKey, SecretKey};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rand::rngs::OsRng;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tmax_crypto::crypto::{
    ENVELOPE_KEY_BYTES, canonical_message_payload, decrypt_with_key, derive_pairwise_key,
    derive_symmetric_key, encrypt_with_key, evm_address_from_public_key, random_key_material,
    sign_payload as crypto_sign_payload, verify_signature,
};
use tmax_crypto::recovery::load_or_create_recovery_key;
use tmax_git::GitMetadata;
use tmax_protocol::{
    AgentMessage, AgentWalletPublic, AttachMode, AttachmentId, AttachmentInfo, ErrorCode, Event,
    OrchestrationWorkflow, SandboxConfig, SessionId, SessionSummary, SharedTask, SharedTaskStatus,
    TaskId, WorkflowId, WorkflowMember,
};
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

const SESSION_THREAD_STACK_BYTES: usize = 64 * 1024;
const RECOVERY_KEY_FILENAME: &str = "tmax-recovery.key";

#[derive(Debug, Clone)]
pub struct SessionManagerConfig {
    pub broadcast_capacity: usize,
    pub max_live_chunks: usize,
    pub history_dir: Option<PathBuf>,
    pub recovery_key_path: Option<PathBuf>,
}

impl Default for SessionManagerConfig {
    fn default() -> Self {
        let history_dir = std::env::var("TMAX_HISTORY_DIR").ok().map(PathBuf::from);
        let recovery_key_path = std::env::var("TMAX_RECOVERY_KEY_PATH")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                history_dir
                    .clone()
                    .map(|dir| dir.join(RECOVERY_KEY_FILENAME))
            });
        Self {
            broadcast_capacity: 256,
            max_live_chunks: 1024,
            history_dir,
            recovery_key_path,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionCreateOptions {
    pub exec: String,
    pub args: Vec<String>,
    pub tags: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub label: Option<String>,
    pub sandbox: Option<SandboxConfig>,
    pub parent_id: Option<SessionId>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub label: Option<String>,
    pub tags: Vec<String>,
    pub exec: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub sandbox: Option<SandboxConfig>,
    pub git: Option<GitMetadata>,
    pub parent_id: Option<SessionId>,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Marker {
    pub name: String,
    pub seq: u64,
    pub timestamp: SystemTime,
}

#[derive(Clone)]
struct AgentWallet {
    session_id: SessionId,
    address: String,
    secret_key: SecretKey,
    public_key: PublicKey,
    created_at_ms: u128,
}

#[derive(Clone)]
struct KeySlotEnvelope {
    ciphertext_b64: String,
    nonce_b64: String,
    ephemeral_public_key_b64: Option<String>,
}

#[derive(Clone)]
struct SessionFilesystemEnvelope {
    agent_slot: KeySlotEnvelope,
    recovery_slot: KeySlotEnvelope,
}

#[derive(Clone)]
struct WorkflowState {
    workflow_id: WorkflowId,
    name: String,
    root_session_id: SessionId,
    members: HashMap<SessionId, WorkflowMemberState>,
    created_at_ms: u128,
    updated_at_ms: u128,
}

#[derive(Clone)]
struct WorkflowMemberState {
    parent_session_id: Option<SessionId>,
    joined_at_ms: u128,
}

impl WorkflowState {
    fn to_public(&self) -> OrchestrationWorkflow {
        let mut members = self
            .members
            .iter()
            .map(|(session_id, member)| WorkflowMember {
                session_id: session_id.clone(),
                parent_session_id: member.parent_session_id.clone(),
                joined_at_ms: member.joined_at_ms,
            })
            .collect::<Vec<_>>();
        members.sort_by_key(|member| member.joined_at_ms);
        OrchestrationWorkflow {
            workflow_id: self.workflow_id.clone(),
            name: self.name.clone(),
            root_session_id: self.root_session_id.clone(),
            members,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

impl AgentWallet {
    fn generate(session_id: &str) -> Result<Self> {
        let signing_key = SigningKey::random(&mut OsRng);
        let secret_key = SecretKey::from_slice(&signing_key.to_bytes())
            .context("failed to construct secp256k1 secret key")?;
        let public_key = secret_key.public_key();
        let created_at_ms = system_time_ms(SystemTime::now());
        let address = evm_address_from_public_key(&public_key);
        Ok(Self {
            session_id: session_id.to_string(),
            address,
            secret_key,
            public_key,
            created_at_ms,
        })
    }

    fn to_public(&self) -> AgentWalletPublic {
        let key_b64 =
            base64::engine::general_purpose::STANDARD.encode(self.public_key.to_sec1_bytes());
        AgentWalletPublic {
            session_id: self.session_id.clone(),
            address: self.address.clone(),
            // EVM-compatible wallets use one secp256k1 keypair for both.
            encryption_public_key_b64: key_b64.clone(),
            signing_public_key_b64: key_b64,
            created_at_ms: self.created_at_ms,
        }
    }
}

pub struct AttachHandle {
    pub info: AttachmentInfo,
    pub replay_events: Vec<Event>,
    pub receiver: broadcast::Receiver<Event>,
}

pub struct Subscription {
    pub replay_events: Vec<Event>,
    pub receiver: broadcast::Receiver<Event>,
}

pub struct Session {
    pub id: SessionId,
    metadata: SessionMetadata,
    master_pty: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    live_buffer: Mutex<LiveBuffer>,
    history_log: Mutex<Option<HistoryLog>>,
    vt_state: Mutex<VtState>,
    markers: Mutex<Vec<Marker>>,
    messages: Mutex<Vec<AgentMessage>>,
    attachments: Mutex<HashMap<AttachmentId, AttachmentInfo>>,
    edit_attachment: Mutex<Option<AttachmentId>>,
    event_tx: broadcast::Sender<Event>,
    exited: AtomicBool,
}

impl Session {
    fn summary(&self) -> SessionSummary {
        SessionSummary {
            session_id: self.id.clone(),
            label: self.metadata.label.clone(),
            tags: self.metadata.tags.clone(),
            exec: self.metadata.exec.clone(),
            cwd: self.metadata.cwd.clone(),
            parent_id: self.metadata.parent_id.clone(),
            created_at_ms: system_time_ms(self.metadata.created_at),
            sandboxed: self.metadata.sandbox.is_some(),
            git_branch: self.metadata.git.as_ref().and_then(|g| g.branch.clone()),
            git_repo_root: self.metadata.git.as_ref().map(|g| g.repo_root.clone()),
            git_worktree_path: self.metadata.git.as_ref().map(|g| g.worktree_path.clone()),
            git_dirty: self.metadata.git.as_ref().map(|g| g.is_dirty),
        }
    }

    fn info(&self, attachment_id: AttachmentId, mode: AttachMode) -> AttachmentInfo {
        AttachmentInfo {
            attachment_id,
            session_id: self.id.clone(),
            mode,
            created_at_ms: system_time_ms(SystemTime::now()),
        }
    }

    fn subscribe(&self, last_seq_seen: Option<u64>) -> Subscription {
        let (snapshot_seq, replay_events) = {
            let live = self.live_buffer.lock().expect("live_buffer poisoned");
            let oldest_seq = live.oldest_seq();
            let newest_seq = live.newest_seq();

            let need_snapshot = match last_seq_seen {
                None => true,
                Some(last) => {
                    let too_old = oldest_seq
                        .map(|old| last.saturating_add(1) < old)
                        .unwrap_or(false);
                    let ahead = newest_seq.map(|new| last > new).unwrap_or(false);
                    too_old || ahead
                }
            };

            let replay_from = if need_snapshot {
                if last_seq_seen.is_some() {
                    newest_seq
                } else {
                    None
                }
            } else {
                last_seq_seen
            };
            let replay_events = live
                .replay_from(replay_from)
                .into_iter()
                .map(|chunk| self.output_event(chunk))
                .collect::<Vec<_>>();
            let snapshot_seq = if need_snapshot {
                Some(newest_seq.or(oldest_seq).unwrap_or(0))
            } else {
                None
            };
            (snapshot_seq, replay_events)
        };

        let mut replay_out = Vec::new();
        if let Some(seq) = snapshot_seq {
            replay_out.push(self.snapshot_event(seq));
        }
        replay_out.extend(replay_events);

        Subscription {
            replay_events: replay_out,
            receiver: self.event_tx.subscribe(),
        }
    }

    fn attach(&self, mode: AttachMode, last_seq_seen: Option<u64>) -> Result<AttachHandle> {
        let attachment_id = Uuid::new_v4().to_string();
        let info = self.info(attachment_id.clone(), mode);

        {
            let mut edit = self.edit_attachment.lock().expect("edit lock poisoned");
            if mode == AttachMode::Edit {
                if edit.is_some() {
                    bail!("edit attachment already exists");
                }
                *edit = Some(attachment_id.clone());
            }
        }

        self.attachments
            .lock()
            .expect("attachments lock poisoned")
            .insert(attachment_id, info.clone());

        let sub = self.subscribe(last_seq_seen);
        Ok(AttachHandle {
            info,
            replay_events: sub.replay_events,
            receiver: sub.receiver,
        })
    }

    fn detach(&self, attachment_id: &str) -> bool {
        let removed = self
            .attachments
            .lock()
            .expect("attachments lock poisoned")
            .remove(attachment_id);

        if let Some(info) = removed {
            if info.mode == AttachMode::Edit {
                let mut edit = self.edit_attachment.lock().expect("edit lock poisoned");
                if edit.as_deref() == Some(attachment_id) {
                    *edit = None;
                }
            }
            return true;
        }

        false
    }

    fn validate_edit_attachment(&self, attachment_id: &str) -> Result<()> {
        let attachments = self.attachments.lock().expect("attachments lock poisoned");
        let info = attachments
            .get(attachment_id)
            .ok_or_else(|| anyhow!("attachment not found"))?;

        if info.mode != AttachMode::Edit {
            bail!("attachment is view-only");
        }

        let edit = self.edit_attachment.lock().expect("edit lock poisoned");
        if edit.as_deref() != Some(attachment_id) {
            bail!("attachment does not own input lock");
        }

        Ok(())
    }

    fn send_input(&self, attachment_id: &str, data: &[u8]) -> Result<()> {
        self.validate_edit_attachment(attachment_id)
            .context("input denied")?;
        let mut writer = self.writer.lock().expect("writer lock poisoned");
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        self.vt_state
            .lock()
            .expect("vt_state lock poisoned")
            .resize(cols, rows);
        self.master_pty
            .lock()
            .expect("master pty lock poisoned")
            .resize(size)
            .context("resize failed")
    }

    fn insert_marker(&self, name: String) -> Result<u64> {
        let seq = self
            .live_buffer
            .lock()
            .expect("live buffer lock poisoned")
            .newest_seq()
            .unwrap_or(0);

        self.markers
            .lock()
            .expect("marker lock poisoned")
            .push(Marker {
                name: name.clone(),
                seq,
                timestamp: SystemTime::now(),
            });

        let event = Event::MarkerInserted {
            session_id: self.id.clone(),
            name,
            seq,
        };
        let _ = self.event_tx.send(event);
        Ok(seq)
    }

    fn list_markers(&self) -> Vec<Marker> {
        self.markers.lock().expect("marker lock poisoned").clone()
    }

    fn push_message(&self, message: AgentMessage) {
        self.messages
            .lock()
            .expect("messages lock poisoned")
            .push(message.clone());
        let _ = self.event_tx.send(Event::MessageReceived {
            session_id: self.id.clone(),
            message,
        });
    }

    fn list_messages(&self, unread_only: bool, limit: Option<usize>) -> Vec<AgentMessage> {
        let messages = self.messages.lock().expect("messages lock poisoned");
        let mut out = messages
            .iter()
            .filter(|m| !unread_only || m.read_at_ms.is_none())
            .cloned()
            .collect::<Vec<_>>();
        if let Some(limit) = limit
            && out.len() > limit
        {
            let start = out.len().saturating_sub(limit);
            out = out[start..].to_vec();
        }
        out
    }

    fn ack_message(&self, message_id: &str) -> Result<AgentMessage> {
        let mut messages = self.messages.lock().expect("messages lock poisoned");
        let message = messages
            .iter_mut()
            .find(|m| m.message_id == message_id)
            .ok_or_else(|| anyhow!("message not found"))?;
        if message.read_at_ms.is_none() {
            message.read_at_ms = Some(system_time_ms(SystemTime::now()));
        }
        Ok(message.clone())
    }

    fn unread_message_count(&self) -> usize {
        self.messages
            .lock()
            .expect("messages lock poisoned")
            .iter()
            .filter(|m| m.read_at_ms.is_none())
            .count()
    }

    fn output_event(&self, chunk: OutputChunk) -> Event {
        Event::Output {
            session_id: self.id.clone(),
            seq: chunk.seq,
            data_b64: base64::engine::general_purpose::STANDARD.encode(chunk.data),
        }
    }

    fn record_output(&self, data: Vec<u8>) {
        let chunk = self
            .live_buffer
            .lock()
            .expect("live buffer lock poisoned")
            .push(data);
        self.vt_state
            .lock()
            .expect("vt_state lock poisoned")
            .feed(&chunk.data);
        if let Some(history) = self
            .history_log
            .lock()
            .expect("history lock poisoned")
            .as_mut()
            && let Err(err) = history.append_chunk(&chunk)
        {
            tracing::warn!(
                "failed to append history log for session {}: {err}",
                self.id
            );
        }
        let event = self.output_event(chunk);
        let _ = self.event_tx.send(event);
    }

    fn snapshot_event(&self, seq: u64) -> Event {
        let snapshot = self
            .vt_state
            .lock()
            .expect("vt_state lock poisoned")
            .snapshot();
        Event::Snapshot {
            session_id: self.id.clone(),
            seq,
            cols: snapshot.cols,
            rows: snapshot.rows,
            lines: snapshot.lines,
        }
    }

    fn record_exit(&self, exit_code: Option<i32>, signal: Option<i32>) {
        if self.exited.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.event_tx.send(Event::SessionExited {
            session_id: self.id.clone(),
            exit_code,
            signal,
        });
    }

    fn kill(&self) -> Result<()> {
        self.killer
            .lock()
            .expect("killer lock poisoned")
            .kill()
            .context("failed to kill child")
    }
}

pub struct SessionManager {
    sessions: RwLock<HashMap<SessionId, Arc<Session>>>,
    session_tree: RwLock<HashMap<SessionId, Vec<SessionId>>>,
    attachment_index: RwLock<HashMap<AttachmentId, SessionId>>,
    wallets: RwLock<HashMap<SessionId, AgentWallet>>,
    fs_envelopes: RwLock<HashMap<SessionId, SessionFilesystemEnvelope>>,
    workflows: RwLock<HashMap<WorkflowId, WorkflowState>>,
    shared_tasks: RwLock<HashMap<TaskId, SharedTask>>,
    recovery_key: [u8; ENVELOPE_KEY_BYTES],
    broker: EventBroker,
    cfg: SessionManagerConfig,
}

impl SessionManager {
    pub fn new(cfg: SessionManagerConfig) -> Self {
        let recovery_key = load_or_create_recovery_key(cfg.recovery_key_path.as_deref())
            .unwrap_or_else(|err| {
                tracing::warn!("failed to load recovery key, falling back to ephemeral key: {err}");
                random_key_material()
            });
        Self {
            sessions: RwLock::new(HashMap::new()),
            session_tree: RwLock::new(HashMap::new()),
            attachment_index: RwLock::new(HashMap::new()),
            wallets: RwLock::new(HashMap::new()),
            fs_envelopes: RwLock::new(HashMap::new()),
            workflows: RwLock::new(HashMap::new()),
            shared_tasks: RwLock::new(HashMap::new()),
            recovery_key,
            broker: EventBroker::new(),
            cfg,
        }
    }

    pub async fn create_session(&self, opts: SessionCreateOptions) -> Result<SessionSummary> {
        let SessionCreateOptions {
            exec,
            args,
            tags,
            cwd: requested_cwd,
            label,
            sandbox,
            parent_id,
            cols,
            rows,
        } = opts;

        let session_id = Uuid::new_v4().to_string();
        let cwd = requested_cwd
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let parent_sandbox = if let Some(parent) = &parent_id {
            let sessions = self.sessions.read().await;
            let parent_session = sessions
                .get(parent)
                .ok_or_else(|| anyhow!("parent session not found"))?;
            parent_session.metadata.sandbox.clone()
        } else {
            None
        };

        let effective_sandbox =
            tmax_sandbox::effective_child_scope(parent_sandbox.as_ref(), sandbox.as_ref(), &cwd)
                .context("invalid sandbox scope")?;
        let git = tmax_git::detect_git_metadata(&cwd).context("git metadata detection failed")?;

        let pty_system = native_pty_system();
        let pty_size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let mut last_pty_err = None;
        let mut pair = None;
        for attempt in 0..5 {
            match pty_system.openpty(pty_size) {
                Ok(opened) => {
                    pair = Some(opened);
                    break;
                }
                Err(err) => {
                    last_pty_err = Some(err);
                    if attempt < 4 {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
        let pair = pair.context("failed to create PTY")?;
        if let Some(err) = last_pty_err {
            tracing::debug!("PTY open retries encountered transient error before success: {err}");
        }

        let (spawn_exec, spawn_args) =
            tmax_sandbox::sandboxed_spawn_command(&exec, &args, effective_sandbox.as_ref())
                .context("failed to prepare sandboxed command")?;

        let mut cmd = CommandBuilder::new(spawn_exec);
        cmd.cwd(cwd.clone());
        for arg in &spawn_args {
            cmd.arg(arg);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn command in PTY")?;
        let killer = child.clone_killer();

        let reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("failed to acquire PTY writer")?;

        let event_tx = self
            .broker
            .register(&session_id, self.cfg.broadcast_capacity)
            .await
            .context("failed to register broker channel")?;
        let history_log = if let Some(history_dir) = &self.cfg.history_dir {
            std::fs::create_dir_all(history_dir).with_context(|| {
                format!("failed to create history dir {}", history_dir.display())
            })?;
            let history_path = history_dir.join(format!("{session_id}.jsonl"));
            Some(HistoryLog::open(&history_path).with_context(|| {
                format!("failed to open history log {}", history_path.display())
            })?)
        } else {
            None
        };
        let session = Arc::new(Session {
            id: session_id.clone(),
            metadata: SessionMetadata {
                label: label.clone(),
                tags,
                exec,
                args,
                cwd,
                sandbox: effective_sandbox,
                git,
                parent_id: parent_id.clone(),
                created_at: SystemTime::now(),
            },
            master_pty: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            live_buffer: Mutex::new(LiveBuffer::new(self.cfg.max_live_chunks)),
            history_log: Mutex::new(history_log),
            vt_state: Mutex::new(VtState::new(cols, rows)),
            markers: Mutex::new(Vec::new()),
            messages: Mutex::new(Vec::new()),
            attachments: Mutex::new(HashMap::new()),
            edit_attachment: Mutex::new(None),
            event_tx,
            exited: AtomicBool::new(false),
        });

        let io_session = Arc::clone(&session);
        thread::Builder::new()
            .name(format!("tmax-io-{}", session_id))
            .stack_size(SESSION_THREAD_STACK_BYTES)
            .spawn(move || {
                let mut reader = reader;
                let mut child = child;
                let mut buf = vec![0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            for chunk in buf[..n].chunks(tmax_protocol::MAX_OUTPUT_CHUNK_BYTES) {
                                io_session.record_output(chunk.to_vec());
                            }
                        }
                        Err(_) => break,
                    }
                }

                match child.wait() {
                    Ok(status) => {
                        let exit_code = i32::try_from(status.exit_code()).ok();
                        let signal = status.signal().and_then(parse_signal_to_i32);
                        io_session.record_exit(exit_code, signal);
                    }
                    Err(_) => io_session.record_exit(None, None),
                }
            })
            .context("failed to spawn PTY io thread")?;

        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_id.clone(), Arc::clone(&session));
        }
        {
            let wallet = AgentWallet::generate(&session_id)?;
            let fs_envelope = create_filesystem_envelope(&wallet, &self.recovery_key)?;
            self.wallets
                .write()
                .await
                .insert(session_id.clone(), wallet);
            self.fs_envelopes
                .write()
                .await
                .insert(session_id.clone(), fs_envelope);
        }

        if let Some(parent_id) = parent_id {
            let mut tree = self.session_tree.write().await;
            tree.entry(parent_id).or_default().push(session_id.clone());
        }

        let _ = session.event_tx.send(Event::SessionCreated {
            session_id: session_id.clone(),
            label: session.metadata.label.clone(),
        });

        Ok(session.summary())
    }

    pub async fn destroy_session(&self, session_id: &str, cascade: bool) -> Result<()> {
        let ids = if cascade {
            self.collect_descendants(session_id).await
        } else {
            vec![session_id.to_string()]
        };

        for id in ids {
            let session = {
                let mut sessions = self.sessions.write().await;
                sessions.remove(&id)
            };

            if let Some(session) = session {
                let _ = session.kill();
                let _ = session.event_tx.send(Event::SessionDestroyed {
                    session_id: id.clone(),
                });

                let attachment_ids: Vec<String> = session
                    .attachments
                    .lock()
                    .expect("attachments lock poisoned")
                    .keys()
                    .cloned()
                    .collect();

                let mut idx = self.attachment_index.write().await;
                for attachment_id in attachment_ids {
                    idx.remove(&attachment_id);
                }
            }

            self.broker.remove(&id).await;
            self.wallets.write().await.remove(&id);
            self.fs_envelopes.write().await.remove(&id);
            self.remove_session_from_workflows(&id).await;

            let mut tree = self.session_tree.write().await;
            tree.remove(&id);
            for children in tree.values_mut() {
                children.retain(|child| child != &id);
            }
        }

        Ok(())
    }

    pub async fn list_sessions(&self) -> Vec<SessionSummary> {
        self.sessions
            .read()
            .await
            .values()
            .map(|s| s.summary())
            .collect()
    }

    pub async fn session_tree(&self) -> HashMap<SessionId, Vec<SessionId>> {
        self.session_tree.read().await.clone()
    }

    pub async fn session_info(&self, session_id: &str) -> Option<SessionSummary> {
        self.sessions
            .read()
            .await
            .get(session_id)
            .map(|s| s.summary())
    }

    pub async fn subscribe(
        &self,
        session_id: &str,
        last_seq_seen: Option<u64>,
    ) -> Result<Subscription> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        Ok(session.subscribe(last_seq_seen))
    }

    pub async fn attach(
        &self,
        session_id: &str,
        mode: AttachMode,
        last_seq_seen: Option<u64>,
    ) -> Result<AttachHandle> {
        let handle = {
            let sessions = self.sessions.read().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| anyhow!("session not found"))?;
            session.attach(mode, last_seq_seen)?
        };

        self.attachment_index
            .write()
            .await
            .insert(handle.info.attachment_id.clone(), session_id.to_string());

        Ok(handle)
    }

    pub async fn detach(&self, attachment_id: &str) -> Result<()> {
        let Some(session_id) = self
            .attachment_index
            .read()
            .await
            .get(attachment_id)
            .cloned()
        else {
            bail!("attachment not found");
        };

        let removed = {
            let sessions = self.sessions.read().await;
            let session = sessions
                .get(&session_id)
                .ok_or_else(|| anyhow!("session not found for attachment"))?;
            session.detach(attachment_id)
        };

        if !removed {
            bail!("attachment not found");
        }

        self.attachment_index.write().await.remove(attachment_id);
        Ok(())
    }

    pub async fn send_input(
        &self,
        session_id: &str,
        attachment_id: &str,
        data: &[u8],
    ) -> Result<()> {
        if data.len() > tmax_protocol::MAX_INPUT_CHUNK_BYTES {
            bail!("input exceeds MAX_INPUT_CHUNK_BYTES");
        }

        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        session.send_input(attachment_id, data)
    }

    pub async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<()> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        session.resize(cols, rows)
    }

    pub async fn insert_marker(&self, session_id: &str, name: String) -> Result<u64> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        session.insert_marker(name)
    }

    pub async fn list_markers(&self, session_id: &str) -> Result<Vec<Marker>> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        Ok(session.list_markers())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_message(
        &self,
        from_session_id: Option<&str>,
        to_session_id: &str,
        topic: Option<String>,
        body: String,
        requires_response: bool,
        encrypt: bool,
        sign: bool,
    ) -> Result<AgentMessage> {
        if body.trim().is_empty() {
            bail!("message body cannot be empty");
        }

        if let Some(from) = from_session_id {
            self.ensure_session_exists(from).await?;
        }

        let recipient = {
            let sessions = self.sessions.read().await;
            sessions
                .get(to_session_id)
                .cloned()
                .ok_or_else(|| anyhow!("recipient session not found"))?
        };
        let sender_wallet = if let Some(from) = from_session_id {
            Some(self.wallet(from).await?)
        } else {
            None
        };
        let recipient_wallet = self.wallet(to_session_id).await?;

        if (encrypt || sign) && sender_wallet.is_none() {
            bail!("from_session_id is required when encrypt/sign is enabled");
        }

        let created_at_ms = system_time_ms(SystemTime::now());
        let canonical_payload = canonical_message_payload(
            from_session_id,
            to_session_id,
            topic.as_deref(),
            &body,
            requires_response,
        );

        let (encrypted, stored_body, ciphertext_b64, nonce_b64) = if encrypt {
            let sender_wallet = sender_wallet
                .as_ref()
                .ok_or_else(|| anyhow!("sender wallet not found"))?;
            let (ciphertext_b64, nonce_b64) =
                encrypt_body(sender_wallet, &recipient_wallet, &body)?;
            (true, String::new(), Some(ciphertext_b64), Some(nonce_b64))
        } else {
            (false, body, None, None)
        };

        let (signature_b64, signer_session_id, signer_public_key_b64) = if sign {
            let sender_wallet = sender_wallet
                .as_ref()
                .ok_or_else(|| anyhow!("sender wallet not found"))?;
            let signature_b64 = sign_payload(sender_wallet, &canonical_payload)?;
            (
                Some(signature_b64),
                from_session_id.map(ToOwned::to_owned),
                Some(
                    base64::engine::general_purpose::STANDARD
                        .encode(sender_wallet.public_key.to_sec1_bytes()),
                ),
            )
        } else {
            (None, None, None)
        };

        let message = AgentMessage {
            message_id: Uuid::new_v4().to_string(),
            from_session_id: from_session_id.map(ToOwned::to_owned),
            to_session_id: to_session_id.to_string(),
            topic,
            encrypted,
            body: stored_body,
            ciphertext_b64,
            nonce_b64,
            signature_b64,
            signer_session_id,
            signer_public_key_b64,
            signature_valid: None,
            requires_response,
            created_at_ms,
            read_at_ms: None,
        };

        recipient.push_message(message.clone());
        Ok(message)
    }

    pub async fn list_messages(
        &self,
        session_id: &str,
        unread_only: bool,
        limit: Option<usize>,
    ) -> Result<Vec<AgentMessage>> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        let raw = session.list_messages(unread_only, limit);
        drop(sessions);

        raw.into_iter()
            .map(|message| self.materialize_message_for_session(session_id, message))
            .collect()
    }

    pub async fn wallet_info(&self, session_id: &str) -> Result<AgentWalletPublic> {
        Ok(self.wallet(session_id).await?.to_public())
    }

    pub async fn decrypt_filesystem_key_as_agent(&self, session_id: &str) -> Result<String> {
        let wallet = self.wallet(session_id).await?;
        let envelope = self.filesystem_envelope(session_id).await?;
        let key = decrypt_key_slot_for_agent(&wallet, &envelope.agent_slot)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(key))
    }

    pub async fn decrypt_filesystem_key_as_recovery(&self, session_id: &str) -> Result<String> {
        self.ensure_session_exists(session_id).await?;
        let envelope = self.filesystem_envelope(session_id).await?;
        let key = decrypt_key_slot_for_recovery(&self.recovery_key, &envelope.recovery_slot)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(key))
    }

    pub async fn ack_message(&self, session_id: &str, message_id: &str) -> Result<AgentMessage> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        session.ack_message(message_id)
    }

    pub async fn unread_message_count(&self, session_id: &str) -> Result<usize> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        Ok(session.unread_message_count())
    }

    pub async fn create_workflow(
        &self,
        name: String,
        root_session_id: SessionId,
    ) -> Result<OrchestrationWorkflow> {
        if name.trim().is_empty() {
            bail!("workflow name cannot be empty");
        }
        self.ensure_session_exists(&root_session_id).await?;

        let now = system_time_ms(SystemTime::now());
        let workflow_id = Uuid::new_v4().to_string();
        let mut members = HashMap::new();
        members.insert(
            root_session_id.clone(),
            WorkflowMemberState {
                parent_session_id: None,
                joined_at_ms: now,
            },
        );
        let state = WorkflowState {
            workflow_id: workflow_id.clone(),
            name: name.trim().to_string(),
            root_session_id: root_session_id.clone(),
            members,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.workflows
            .write()
            .await
            .insert(workflow_id, state.clone());
        let workflow = state.to_public();
        self.broadcast_workflow_event(workflow.clone()).await;
        Ok(workflow)
    }

    pub async fn join_workflow(
        &self,
        workflow_id: &str,
        session_id: &str,
        parent_session_id: &str,
    ) -> Result<OrchestrationWorkflow> {
        self.ensure_session_exists(session_id).await?;
        self.ensure_session_exists(parent_session_id).await?;

        let mut workflows = self.workflows.write().await;
        let workflow = workflows
            .get_mut(workflow_id)
            .ok_or_else(|| anyhow!("workflow not found"))?;
        if !workflow.members.contains_key(parent_session_id) {
            bail!("parent session is not a workflow member");
        }

        let now = system_time_ms(SystemTime::now());
        workflow.members.insert(
            session_id.to_string(),
            WorkflowMemberState {
                parent_session_id: Some(parent_session_id.to_string()),
                joined_at_ms: now,
            },
        );
        workflow.updated_at_ms = now;
        let updated = workflow.to_public();
        drop(workflows);
        self.broadcast_workflow_event(updated.clone()).await;
        Ok(updated)
    }

    pub async fn leave_workflow(
        &self,
        workflow_id: &str,
        session_id: &str,
    ) -> Result<OrchestrationWorkflow> {
        let mut workflows = self.workflows.write().await;
        let workflow = workflows
            .get_mut(workflow_id)
            .ok_or_else(|| anyhow!("workflow not found"))?;

        if workflow.root_session_id == session_id {
            bail!("workflow root cannot leave workflow");
        }
        if workflow
            .members
            .values()
            .any(|member| member.parent_session_id.as_deref() == Some(session_id))
        {
            bail!("workflow member has child members; remove children first");
        }
        if workflow.members.remove(session_id).is_none() {
            bail!("workflow member not found");
        }
        workflow.updated_at_ms = system_time_ms(SystemTime::now());
        let updated = workflow.to_public();
        drop(workflows);
        self.broadcast_workflow_event(updated.clone()).await;
        Ok(updated)
    }

    pub async fn list_workflows(&self, session_id: &str) -> Result<Vec<OrchestrationWorkflow>> {
        self.ensure_session_exists(session_id).await?;
        let mut out = self
            .workflows
            .read()
            .await
            .values()
            .filter(|workflow| workflow.members.contains_key(session_id))
            .map(WorkflowState::to_public)
            .collect::<Vec<_>>();
        out.sort_by_key(|workflow| (workflow.created_at_ms, workflow.workflow_id.clone()));
        Ok(out)
    }

    pub async fn create_shared_task(
        &self,
        workflow_id: String,
        title: String,
        description: Option<String>,
        created_by: SessionId,
        depends_on: Vec<TaskId>,
    ) -> Result<SharedTask> {
        if title.trim().is_empty() {
            bail!("task title cannot be empty");
        }
        self.ensure_session_exists(&created_by).await?;
        self.ensure_workflow_member(&workflow_id, &created_by)
            .await?;

        let mut tasks = self.shared_tasks.write().await;
        for dep in &depends_on {
            let dep_task = tasks
                .get(dep)
                .ok_or_else(|| anyhow!("dependency task not found: {dep}"))?;
            if dep_task.workflow_id != workflow_id {
                bail!("dependency task is in a different workflow");
            }
        }

        let now = system_time_ms(SystemTime::now());
        let status = if depends_on.is_empty() {
            SharedTaskStatus::Todo
        } else {
            SharedTaskStatus::Blocked
        };
        let task = SharedTask {
            task_id: Uuid::new_v4().to_string(),
            workflow_id,
            title: title.trim().to_string(),
            description,
            status,
            created_by: Some(created_by),
            assignee_session_id: None,
            depends_on,
            created_at_ms: now,
            updated_at_ms: now,
            completed_at_ms: None,
        };
        tasks.insert(task.task_id.clone(), task.clone());
        drop(tasks);

        self.recompute_blocked_tasks().await?;
        self.broadcast_task_event(task.clone()).await;
        Ok(task)
    }

    pub async fn list_shared_tasks(
        &self,
        workflow_id: &str,
        session_id: &str,
        include_done: bool,
    ) -> Result<Vec<SharedTask>> {
        self.ensure_workflow_member(workflow_id, session_id).await?;
        let mut tasks = self
            .shared_tasks
            .read()
            .await
            .values()
            .filter(|task| task.workflow_id == workflow_id)
            .filter(|task| include_done || task.status != SharedTaskStatus::Done)
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| (task.created_at_ms, task.task_id.clone()));
        Ok(tasks)
    }

    pub async fn claim_shared_task(&self, task_id: &str, session_id: &str) -> Result<SharedTask> {
        self.ensure_session_exists(session_id).await?;
        let tasks = self.shared_tasks.read().await;
        let workflow_id = tasks
            .get(task_id)
            .ok_or_else(|| anyhow!("task not found"))?
            .workflow_id
            .clone();
        drop(tasks);
        self.ensure_workflow_member(&workflow_id, session_id)
            .await?;

        let mut tasks = self.shared_tasks.write().await;
        let deps_ready = {
            let task = tasks
                .get(task_id)
                .ok_or_else(|| anyhow!("task not found"))?;
            deps_completed(task, &tasks)?
        };

        let now = system_time_ms(SystemTime::now());
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("task not found"))?;
        task.assignee_session_id = Some(session_id.to_string());
        if task.status != SharedTaskStatus::Done {
            task.status = if deps_ready {
                SharedTaskStatus::InProgress
            } else {
                SharedTaskStatus::Blocked
            };
            task.completed_at_ms = None;
        }
        task.updated_at_ms = now;
        let updated = task.clone();
        drop(tasks);

        self.broadcast_task_event(updated.clone()).await;
        Ok(updated)
    }

    pub async fn set_shared_task_status(
        &self,
        task_id: &str,
        session_id: &str,
        status: SharedTaskStatus,
    ) -> Result<SharedTask> {
        self.ensure_session_exists(session_id).await?;
        let tasks = self.shared_tasks.read().await;
        let workflow_id = tasks
            .get(task_id)
            .ok_or_else(|| anyhow!("task not found"))?
            .workflow_id
            .clone();
        drop(tasks);
        self.ensure_workflow_member(&workflow_id, session_id)
            .await?;

        let mut tasks = self.shared_tasks.write().await;
        let deps_ready = {
            let task = tasks
                .get(task_id)
                .ok_or_else(|| anyhow!("task not found"))?;
            deps_completed(task, &tasks)?
        };

        if matches!(
            status,
            SharedTaskStatus::InProgress | SharedTaskStatus::Done
        ) && !deps_ready
        {
            bail!("task has incomplete dependencies");
        }

        let now = system_time_ms(SystemTime::now());
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("task not found"))?;
        task.status = status;
        task.updated_at_ms = now;
        task.completed_at_ms = if status == SharedTaskStatus::Done {
            Some(now)
        } else {
            None
        };

        let updated = task.clone();
        drop(tasks);

        self.recompute_blocked_tasks().await?;
        self.broadcast_task_event(updated.clone()).await;
        Ok(updated)
    }

    pub async fn task_by_id(&self, task_id: &str) -> Option<SharedTask> {
        self.shared_tasks.read().await.get(task_id).cloned()
    }

    async fn collect_descendants(&self, root: &str) -> Vec<String> {
        let tree = self.session_tree.read().await;
        let mut stack = vec![root.to_string()];
        let mut out = Vec::new();
        while let Some(current) = stack.pop() {
            out.push(current.clone());
            if let Some(children) = tree.get(&current) {
                for child in children {
                    stack.push(child.clone());
                }
            }
        }
        out
    }

    async fn ensure_session_exists(&self, session_id: &str) -> Result<()> {
        let sessions = self.sessions.read().await;
        if !sessions.contains_key(session_id) {
            bail!("session not found");
        }
        Ok(())
    }

    async fn wallet(&self, session_id: &str) -> Result<AgentWallet> {
        let wallets = self.wallets.read().await;
        wallets
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("wallet not found for session"))
    }

    async fn filesystem_envelope(&self, session_id: &str) -> Result<SessionFilesystemEnvelope> {
        let envelopes = self.fs_envelopes.read().await;
        envelopes
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("filesystem envelope not found for session"))
    }

    fn materialize_message_for_session(
        &self,
        session_id: &str,
        mut message: AgentMessage,
    ) -> Result<AgentMessage> {
        let body = if message.encrypted {
            let ciphertext_b64 = message
                .ciphertext_b64
                .as_deref()
                .ok_or_else(|| anyhow!("encrypted message missing ciphertext"))?;
            let nonce_b64 = message
                .nonce_b64
                .as_deref()
                .ok_or_else(|| anyhow!("encrypted message missing nonce"))?;
            let sender_key_b64 = message
                .signer_public_key_b64
                .as_deref()
                .ok_or_else(|| anyhow!("encrypted message missing sender public key"))?;
            decrypt_body(
                self.wallet_blocking(session_id)?,
                sender_key_b64,
                ciphertext_b64,
                nonce_b64,
            )?
        } else {
            message.body.clone()
        };

        message.body = body.clone();
        message.signature_valid = if let Some(signature_b64) = &message.signature_b64 {
            let signer_key_b64 = if let Some(key) = &message.signer_public_key_b64 {
                key.clone()
            } else if let Some(signer_session_id) = &message.signer_session_id {
                let wallet = self.wallet_blocking(signer_session_id)?;
                base64::engine::general_purpose::STANDARD.encode(wallet.public_key.to_sec1_bytes())
            } else {
                String::new()
            };
            if signer_key_b64.is_empty() {
                Some(false)
            } else {
                let payload = canonical_message_payload(
                    message.from_session_id.as_deref(),
                    &message.to_session_id,
                    message.topic.as_deref(),
                    &body,
                    message.requires_response,
                );
                Some(verify_signature(&signer_key_b64, &payload, signature_b64))
            }
        } else {
            None
        };
        Ok(message)
    }

    fn wallet_blocking(&self, session_id: &str) -> Result<AgentWallet> {
        self.wallets
            .try_read()
            .map_err(|_| anyhow!("wallet lock busy"))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("wallet not found for session"))
    }

    async fn ensure_workflow_member(&self, workflow_id: &str, session_id: &str) -> Result<()> {
        let workflows = self.workflows.read().await;
        let workflow = workflows
            .get(workflow_id)
            .ok_or_else(|| anyhow!("workflow not found"))?;
        if !workflow.members.contains_key(session_id) {
            bail!("session is not a workflow member");
        }
        Ok(())
    }

    async fn remove_session_from_workflows(&self, session_id: &str) {
        let removed_roots = {
            let mut workflows = self.workflows.write().await;
            let removed_roots = workflows
                .iter()
                .filter_map(|(workflow_id, workflow)| {
                    (workflow.root_session_id == session_id).then_some(workflow_id.clone())
                })
                .collect::<Vec<_>>();

            for workflow_id in &removed_roots {
                workflows.remove(workflow_id);
            }

            for workflow in workflows.values_mut() {
                if workflow.members.remove(session_id).is_none() {
                    continue;
                }
                for member in workflow.members.values_mut() {
                    if member.parent_session_id.as_deref() == Some(session_id) {
                        member.parent_session_id = Some(workflow.root_session_id.clone());
                    }
                }
                workflow.updated_at_ms = system_time_ms(SystemTime::now());
            }
            removed_roots
        };

        for workflow_id in removed_roots {
            self.remove_workflow_tasks(&workflow_id).await;
        }
    }

    async fn remove_workflow_tasks(&self, workflow_id: &str) {
        let mut tasks = self.shared_tasks.write().await;
        tasks.retain(|_, task| task.workflow_id != workflow_id);
    }

    async fn broadcast_workflow_event(&self, workflow: OrchestrationWorkflow) {
        let event = Event::WorkflowUpdated { workflow };
        let sessions = self.sessions.read().await;
        for session in sessions.values() {
            let _ = session.event_tx.send(event.clone());
        }
    }

    async fn broadcast_task_event(&self, task: SharedTask) {
        let workflow_id = task.workflow_id.clone();
        let event = Event::TaskUpdated { task };
        let member_ids = {
            let workflows = self.workflows.read().await;
            workflows
                .get(&workflow_id)
                .map(|w| w.members.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let sessions = self.sessions.read().await;
        for member_id in member_ids {
            if let Some(session) = sessions.get(&member_id) {
                let _ = session.event_tx.send(event.clone());
            }
        }
    }

    async fn recompute_blocked_tasks(&self) -> Result<()> {
        let mut tasks = self.shared_tasks.write().await;
        let now = system_time_ms(SystemTime::now());
        let task_ids = tasks.keys().cloned().collect::<Vec<_>>();
        let mut changed = Vec::new();
        for task_id in task_ids {
            let should_unblock = if let Some(task) = tasks.get(&task_id) {
                task.status == SharedTaskStatus::Blocked && deps_completed(task, &tasks)?
            } else {
                false
            };
            if should_unblock && let Some(task) = tasks.get_mut(&task_id) {
                task.status = SharedTaskStatus::Todo;
                task.updated_at_ms = now;
                changed.push(task.clone());
            }
        }
        drop(tasks);
        for task in changed {
            self.broadcast_task_event(task).await;
        }
        Ok(())
    }

    pub fn map_err_code(err: &anyhow::Error) -> ErrorCode {
        let msg = err.to_string();
        if msg.contains("not found") {
            return ErrorCode::NotFound;
        }
        if msg.contains("denied")
            || msg.contains("view-only")
            || msg.contains("not a workflow member")
        {
            return ErrorCode::PermissionDenied;
        }
        if msg.contains("exists") || msg.contains("conflict") {
            return ErrorCode::Conflict;
        }
        if msg.contains("incomplete dependencies") {
            return ErrorCode::Conflict;
        }
        ErrorCode::Internal
    }
}

fn create_filesystem_envelope(
    wallet: &AgentWallet,
    recovery_key: &[u8; ENVELOPE_KEY_BYTES],
) -> Result<SessionFilesystemEnvelope> {
    let fs_key = random_key_material();
    let agent_slot = encrypt_key_for_agent_wallet(wallet, &fs_key)?;
    let recovery_slot = encrypt_key_for_recovery(recovery_key, &fs_key)?;
    Ok(SessionFilesystemEnvelope {
        agent_slot,
        recovery_slot,
    })
}

fn encrypt_key_for_agent_wallet(
    wallet: &AgentWallet,
    fs_key: &[u8; ENVELOPE_KEY_BYTES],
) -> Result<KeySlotEnvelope> {
    let ephemeral = SecretKey::random(&mut OsRng);
    let shared = diffie_hellman(ephemeral.to_nonzero_scalar(), wallet.public_key.as_affine());
    let slot_key = derive_symmetric_key(
        b"tmax-fs-slot-agent-v1",
        shared.raw_secret_bytes().as_slice(),
    );
    let (ciphertext_b64, nonce_b64) = encrypt_with_key(&slot_key, fs_key)?;
    Ok(KeySlotEnvelope {
        ciphertext_b64,
        nonce_b64,
        ephemeral_public_key_b64: Some(
            base64::engine::general_purpose::STANDARD
                .encode(ephemeral.public_key().to_sec1_bytes()),
        ),
    })
}

fn decrypt_key_slot_for_agent(
    wallet: &AgentWallet,
    slot: &KeySlotEnvelope,
) -> Result<[u8; ENVELOPE_KEY_BYTES]> {
    let ephemeral_b64 = slot
        .ephemeral_public_key_b64
        .as_deref()
        .ok_or_else(|| anyhow!("agent slot missing ephemeral key"))?;
    let ephemeral_bytes = base64::engine::general_purpose::STANDARD
        .decode(ephemeral_b64)
        .context("agent slot ephemeral key is not valid base64")?;
    let ephemeral_pub = PublicKey::from_sec1_bytes(&ephemeral_bytes)
        .context("agent slot ephemeral key is not valid secp256k1 key")?;
    let shared = diffie_hellman(
        wallet.secret_key.to_nonzero_scalar(),
        ephemeral_pub.as_affine(),
    );
    let slot_key = derive_symmetric_key(
        b"tmax-fs-slot-agent-v1",
        shared.raw_secret_bytes().as_slice(),
    );
    let plaintext = decrypt_with_key(&slot_key, &slot.ciphertext_b64, &slot.nonce_b64)?;
    if plaintext.len() != ENVELOPE_KEY_BYTES {
        bail!("agent slot plaintext key length mismatch");
    }
    let mut out = [0u8; ENVELOPE_KEY_BYTES];
    out.copy_from_slice(&plaintext);
    Ok(out)
}

fn encrypt_key_for_recovery(
    recovery_key: &[u8; ENVELOPE_KEY_BYTES],
    fs_key: &[u8; ENVELOPE_KEY_BYTES],
) -> Result<KeySlotEnvelope> {
    let slot_key = derive_symmetric_key(b"tmax-fs-slot-recovery-v1", recovery_key);
    let (ciphertext_b64, nonce_b64) = encrypt_with_key(&slot_key, fs_key)?;
    Ok(KeySlotEnvelope {
        ciphertext_b64,
        nonce_b64,
        ephemeral_public_key_b64: None,
    })
}

fn decrypt_key_slot_for_recovery(
    recovery_key: &[u8; ENVELOPE_KEY_BYTES],
    slot: &KeySlotEnvelope,
) -> Result<[u8; ENVELOPE_KEY_BYTES]> {
    let slot_key = derive_symmetric_key(b"tmax-fs-slot-recovery-v1", recovery_key);
    let plaintext = decrypt_with_key(&slot_key, &slot.ciphertext_b64, &slot.nonce_b64)?;
    if plaintext.len() != ENVELOPE_KEY_BYTES {
        bail!("recovery slot plaintext key length mismatch");
    }
    let mut out = [0u8; ENVELOPE_KEY_BYTES];
    out.copy_from_slice(&plaintext);
    Ok(out)
}

fn encrypt_body(
    sender_wallet: &AgentWallet,
    recipient_wallet: &AgentWallet,
    body: &str,
) -> Result<(String, String)> {
    let key = derive_pairwise_key(&sender_wallet.secret_key, &recipient_wallet.public_key);
    encrypt_with_key(&key, body.as_bytes())
}

fn decrypt_body(
    recipient_wallet: AgentWallet,
    sender_public_key_b64: &str,
    ciphertext_b64: &str,
    nonce_b64: &str,
) -> Result<String> {
    let sender_pub_bytes = base64::engine::general_purpose::STANDARD
        .decode(sender_public_key_b64)
        .context("sender public key is not valid base64")?;
    let sender_pub = PublicKey::from_sec1_bytes(&sender_pub_bytes)
        .context("sender public key is not valid secp256k1 key")?;
    let key = derive_pairwise_key(&recipient_wallet.secret_key, &sender_pub);
    let plaintext = decrypt_with_key(&key, ciphertext_b64, nonce_b64)?;
    String::from_utf8(plaintext).context("decrypted plaintext is not utf-8")
}

fn sign_payload(wallet: &AgentWallet, payload: &[u8]) -> Result<String> {
    crypto_sign_payload(&wallet.secret_key, payload)
}

fn deps_completed(task: &SharedTask, tasks: &HashMap<TaskId, SharedTask>) -> Result<bool> {
    for dep in &task.depends_on {
        let dep_task = tasks
            .get(dep)
            .ok_or_else(|| anyhow!("dependency task not found: {dep}"))?;
        if dep_task.status != SharedTaskStatus::Done {
            return Ok(false);
        }
    }
    Ok(true)
}

fn system_time_ms(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn parse_signal_to_i32(signal: &str) -> Option<i32> {
    if let Some(rest) = signal.strip_prefix("SIG") {
        return rest.parse::<i32>().ok();
    }
    signal.parse::<i32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use base64::Engine;
    use std::path::Path;
    use tempfile::tempdir;
    use tmax_protocol::{MAX_INPUT_CHUNK_BYTES, MAX_OUTPUT_CHUNK_BYTES, SharedTaskStatus};
    use tokio::time::{Duration, sleep, timeout};

    fn sh_cmd(script: &str) -> SessionCreateOptions {
        SessionCreateOptions {
            exec: "/bin/sh".to_string(),
            args: vec!["-lc".to_string(), script.to_string()],
            tags: Vec::new(),
            cwd: None,
            label: None,
            sandbox: None,
            parent_id: None,
            cols: 80,
            rows: 24,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn create_and_replay_output() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager.create_session(sh_cmd("echo hello")).await?;
        sleep(Duration::from_millis(200)).await;
        let sub = manager.subscribe(&session.session_id, None).await?;
        let found = sub.replay_events.iter().any(|event| {
            if let Event::Output { data_b64, .. } = event
                && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data_b64)
            {
                return String::from_utf8_lossy(&bytes).contains("hello");
            }
            false
        });
        assert!(found, "expected replay to contain hello");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn view_attachment_cannot_send_input() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager.create_session(sh_cmd("cat")).await?;

        let view = manager
            .attach(&session.session_id, AttachMode::View, None)
            .await?;
        let err = manager
            .send_input(&session.session_id, &view.info.attachment_id, b"oops")
            .await
            .expect_err("view mode must not send input");
        assert!(err.to_string().contains("denied") || err.to_string().contains("view-only"));

        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_subscribers_get_same_replay() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager
            .create_session(sh_cmd("echo alpha; echo beta"))
            .await?;
        sleep(Duration::from_millis(200)).await;

        let s1 = manager.subscribe(&session.session_id, None).await?;
        let s2 = manager.subscribe(&session.session_id, None).await?;
        assert_eq!(s1.replay_events.len(), s2.replay_events.len());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn emits_session_exited_event() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager.create_session(sh_cmd("sleep 0.2")).await?;

        let mut sub = manager.subscribe(&session.session_id, None).await?;
        let mut seen = false;
        let _ = timeout(Duration::from_secs(3), async {
            loop {
                match sub.receiver.recv().await {
                    Ok(Event::SessionExited { session_id, .. }) => {
                        if session_id == session.session_id {
                            seen = true;
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        })
        .await;

        assert!(seen, "expected SessionExited event for the session");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_chunks_respect_protocol_limit() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager
            .create_session(sh_cmd("head -c 50000 /dev/zero | tr '\\\\0' 'a'"))
            .await?;
        sleep(Duration::from_millis(400)).await;

        let sub = manager.subscribe(&session.session_id, None).await?;
        let output_events: Vec<_> = sub
            .replay_events
            .iter()
            .filter_map(|event| {
                if let Event::Output { data_b64, .. } = event {
                    Some(data_b64)
                } else {
                    None
                }
            })
            .collect();

        assert!(!output_events.is_empty(), "expected output replay");
        for data_b64 in output_events {
            let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
            assert!(
                bytes.len() <= MAX_OUTPUT_CHUNK_BYTES,
                "chunk exceeded MAX_OUTPUT_CHUNK_BYTES: {} > {}",
                bytes.len(),
                MAX_OUTPUT_CHUNK_BYTES
            );
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconnect_catches_up_from_last_seq() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager
            .create_session(sh_cmd("echo one; echo two; echo three"))
            .await?;
        sleep(Duration::from_millis(300)).await;

        let first = manager.subscribe(&session.session_id, None).await?;
        let max_seq = first
            .replay_events
            .iter()
            .filter_map(|event| match event {
                Event::Output { seq, .. } => Some(*seq),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        assert!(max_seq > 0, "expected at least one output event");

        let second = manager
            .subscribe(&session.session_id, Some(max_seq.saturating_sub(1)))
            .await?;
        let replay_output_count = second
            .replay_events
            .iter()
            .filter(|event| matches!(event, Event::Output { .. }))
            .count();
        assert_eq!(
            replay_output_count, 1,
            "expected replay from last seq cursor"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_nesting_and_cascade_destroy() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let parent = manager.create_session(sh_cmd("sleep 5")).await?;
        let child = manager
            .create_session(SessionCreateOptions {
                parent_id: Some(parent.session_id.clone()),
                ..sh_cmd("sleep 5")
            })
            .await?;

        let tree = manager.session_tree().await;
        let children = tree.get(&parent.session_id).cloned().unwrap_or_default();
        assert!(children.contains(&child.session_id));

        manager.destroy_session(&parent.session_id, true).await?;
        let sessions = manager.list_sessions().await;
        assert!(!sessions.iter().any(|s| s.session_id == parent.session_id));
        assert!(!sessions.iter().any(|s| s.session_id == child.session_id));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_one_edit_attachment_allowed() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager.create_session(sh_cmd("cat")).await?;
        let _first = manager
            .attach(&session.session_id, AttachMode::Edit, None)
            .await?;
        let second = manager
            .attach(&session.session_id, AttachMode::Edit, None)
            .await;
        assert!(second.is_err(), "second edit attachment should be rejected");
        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nested_sandbox_subset_is_allowed() -> Result<()> {
        let root = tempdir()?;
        let parent_path = root.path().join("parent");
        let child_path = parent_path.join("child");
        std::fs::create_dir_all(&child_path)?;

        let parent = SandboxConfig {
            writable_paths: vec![parent_path],
            readable_paths: vec![],
        };
        let child = SandboxConfig {
            writable_paths: vec![child_path],
            readable_paths: vec![],
        };
        let effective =
            tmax_sandbox::effective_child_scope(Some(&parent), Some(&child), Path::new("/"))?;
        assert!(effective.is_some(), "expected child scope to be accepted");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nested_sandbox_outside_parent_is_rejected() -> Result<()> {
        let root = tempdir()?;
        let parent_path = root.path().join("parent");
        let outside_path = root.path().join("outside");
        std::fs::create_dir_all(&parent_path)?;
        std::fs::create_dir_all(&outside_path)?;

        let parent = SandboxConfig {
            writable_paths: vec![parent_path],
            readable_paths: vec![],
        };
        let child = SandboxConfig {
            writable_paths: vec![outside_path],
            readable_paths: vec![],
        };
        let err = tmax_sandbox::effective_child_scope(Some(&parent), Some(&child), Path::new("/"))
            .expect_err("outside scope should be rejected");
        assert!(err.to_string().contains("outside parent scope"));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn macos_sandbox_allows_inside_and_blocks_outside_writes() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let root = tempdir()?;
        let writable = root.path().join("writable");
        let blocked = root.path().join("blocked");
        std::fs::create_dir_all(&writable)?;
        std::fs::create_dir_all(&blocked)?;

        let inside_file = writable.join("inside.txt");
        let outside_file = blocked.join("outside.txt");
        let script = format!(
            "echo inside > \"{}\"; echo outside > \"{}\"",
            inside_file.display(),
            outside_file.display()
        );

        let session = manager
            .create_session(SessionCreateOptions {
                sandbox: Some(SandboxConfig {
                    writable_paths: vec![writable.clone()],
                    readable_paths: vec![],
                }),
                ..sh_cmd(&script)
            })
            .await?;

        sleep(Duration::from_millis(600)).await;
        assert!(inside_file.exists(), "inside write should succeed");
        assert!(
            !outside_file.exists(),
            "outside write should be blocked by sandbox"
        );

        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_summary_includes_git_metadata_when_repo_detected() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let root = tempdir()?;
        let _repo = git2::Repository::init(root.path())?;

        let session = manager
            .create_session(SessionCreateOptions {
                cwd: Some(root.path().to_path_buf()),
                ..sh_cmd("echo git")
            })
            .await?;

        let expected = std::fs::canonicalize(root.path())?;
        let repo_root = std::fs::canonicalize(
            session
                .git_repo_root
                .as_deref()
                .expect("repo root should be present"),
        )?;
        let worktree = std::fs::canonicalize(
            session
                .git_worktree_path
                .as_deref()
                .expect("worktree should be present"),
        )?;
        assert_eq!(repo_root, expected);
        assert_eq!(worktree, expected);
        assert_eq!(session.git_dirty, Some(false));

        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_input_over_max_chunk_size() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager.create_session(sh_cmd("cat")).await?;
        let edit = manager
            .attach(&session.session_id, AttachMode::Edit, None)
            .await?;

        let oversized = vec![b'a'; MAX_INPUT_CHUNK_BYTES + 1];
        let err = manager
            .send_input(&session.session_id, &edit.info.attachment_id, &oversized)
            .await
            .expect_err("oversized input should fail");
        assert!(err.to_string().contains("MAX_INPUT_CHUNK_BYTES"));

        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn appends_history_log_when_enabled() -> Result<()> {
        let history_dir = tempdir()?;
        let manager = SessionManager::new(SessionManagerConfig {
            history_dir: Some(history_dir.path().to_path_buf()),
            ..SessionManagerConfig::default()
        });

        let session = manager.create_session(sh_cmd("echo history")).await?;
        sleep(Duration::from_millis(250)).await;

        let log_path = history_dir
            .path()
            .join(format!("{}.jsonl", session.session_id));
        let content = std::fs::read_to_string(&log_path)
            .with_context(|| format!("expected history log at {}", log_path.display()))?;
        let mut saw_history = false;
        for line in content.lines() {
            let value: serde_json::Value = serde_json::from_str(line)?;
            if let Some(data_b64) = value.get("data_b64").and_then(|v| v.as_str()) {
                let decoded = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
                if String::from_utf8_lossy(&decoded).contains("history") {
                    saw_history = true;
                    break;
                }
            }
        }
        assert!(saw_history, "expected history output in history log");

        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconnect_with_stale_seq_gets_snapshot_resync() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig {
            max_live_chunks: 1,
            ..SessionManagerConfig::default()
        });
        let session = manager
            .create_session(sh_cmd("head -c 50000 /dev/zero | tr '\\\\0' 'a'"))
            .await?;
        sleep(Duration::from_millis(500)).await;

        let sub = manager.subscribe(&session.session_id, Some(1)).await?;
        let has_snapshot = sub
            .replay_events
            .iter()
            .any(|e| matches!(e, Event::Snapshot { session_id, .. } if session_id == &session.session_id));
        assert!(has_snapshot, "stale seq should force snapshot resync");
        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconnect_with_recent_seq_replays_without_snapshot() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager
            .create_session(sh_cmd("echo alpha; echo beta; echo gamma"))
            .await?;
        sleep(Duration::from_millis(300)).await;
        let first = manager.subscribe(&session.session_id, None).await?;
        let max_seq = first
            .replay_events
            .iter()
            .filter_map(|event| match event {
                Event::Output { seq, .. } => Some(*seq),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        assert!(max_seq > 0);

        let sub = manager
            .subscribe(&session.session_id, Some(max_seq.saturating_sub(1)))
            .await?;
        let has_snapshot = sub
            .replay_events
            .iter()
            .any(|e| matches!(e, Event::Snapshot { .. }));
        assert!(!has_snapshot, "recent seq should replay deltas only");
        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn message_inbox_send_list_ack_and_count() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let sender = manager.create_session(sh_cmd("sleep 5")).await?;
        let recipient = manager.create_session(sh_cmd("sleep 5")).await?;

        let message = manager
            .send_message(
                Some(&sender.session_id),
                &recipient.session_id,
                Some("question".to_string()),
                "Need clarification on target API version".to_string(),
                true,
                false,
                false,
            )
            .await?;
        assert_eq!(
            message.from_session_id.as_deref(),
            Some(sender.session_id.as_str())
        );

        let unread = manager
            .list_messages(&recipient.session_id, true, None)
            .await?;
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].message_id, message.message_id);

        let unread_count = manager.unread_message_count(&recipient.session_id).await?;
        assert_eq!(unread_count, 1);

        let acked = manager
            .ack_message(&recipient.session_id, &message.message_id)
            .await?;
        assert!(acked.read_at_ms.is_some(), "ack should stamp read_at_ms");

        let unread_count = manager.unread_message_count(&recipient.session_id).await?;
        assert_eq!(unread_count, 0);

        manager.destroy_session(&sender.session_id, false).await?;
        manager
            .destroy_session(&recipient.session_id, false)
            .await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn receiving_message_emits_session_event() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let recipient = manager.create_session(sh_cmd("sleep 5")).await?;

        let mut sub = manager.subscribe(&recipient.session_id, None).await?;
        manager
            .send_message(
                None,
                &recipient.session_id,
                Some("status".to_string()),
                "ping".to_string(),
                false,
                false,
                false,
            )
            .await?;

        let mut saw = false;
        let _ = timeout(Duration::from_secs(2), async {
            loop {
                match sub.receiver.recv().await {
                    Ok(Event::MessageReceived {
                        session_id,
                        message,
                    }) if session_id == recipient.session_id && message.body == "ping" => {
                        saw = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        })
        .await;
        assert!(saw, "expected message-received event");

        manager
            .destroy_session(&recipient.session_id, false)
            .await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn encrypted_signed_message_round_trip() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let sender = manager.create_session(sh_cmd("sleep 5")).await?;
        let recipient = manager.create_session(sh_cmd("sleep 5")).await?;

        let sent = manager
            .send_message(
                Some(&sender.session_id),
                &recipient.session_id,
                Some("secure".to_string()),
                "secret payload".to_string(),
                true,
                true,
                true,
            )
            .await?;
        assert!(sent.encrypted);
        assert!(sent.ciphertext_b64.is_some());
        assert!(sent.signature_b64.is_some());

        let listed = manager
            .list_messages(&recipient.session_id, false, None)
            .await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].body, "secret payload");
        assert_eq!(listed[0].signature_valid, Some(true));

        manager.destroy_session(&sender.session_id, false).await?;
        manager
            .destroy_session(&recipient.session_id, false)
            .await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn filesystem_key_dual_decrypt_matches() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let session = manager.create_session(sh_cmd("sleep 5")).await?;

        let agent_key = manager
            .decrypt_filesystem_key_as_agent(&session.session_id)
            .await?;
        let recovery_key = manager
            .decrypt_filesystem_key_as_recovery(&session.session_id)
            .await?;
        assert_eq!(agent_key, recovery_key);

        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_tasks_support_dependencies_and_claims() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig::default());
        let worker = manager.create_session(sh_cmd("sleep 5")).await?;
        let collaborator = manager.create_session(sh_cmd("sleep 5")).await?;
        let workflow = manager
            .create_workflow("coordination".to_string(), worker.session_id.clone())
            .await?;
        let _ = manager
            .join_workflow(
                &workflow.workflow_id,
                &collaborator.session_id,
                &worker.session_id,
            )
            .await?;

        let parent = manager
            .create_shared_task(
                workflow.workflow_id.clone(),
                "Collect API candidates".to_string(),
                None,
                worker.session_id.clone(),
                Vec::new(),
            )
            .await?;
        assert_eq!(parent.status, SharedTaskStatus::Todo);

        let child = manager
            .create_shared_task(
                workflow.workflow_id.clone(),
                "Draft recommendation".to_string(),
                Some("depends on candidate list".to_string()),
                worker.session_id.clone(),
                vec![parent.task_id.clone()],
            )
            .await?;
        assert_eq!(child.status, SharedTaskStatus::Blocked);

        let claimed_blocked = manager
            .claim_shared_task(&child.task_id, &collaborator.session_id)
            .await?;
        assert_eq!(claimed_blocked.status, SharedTaskStatus::Blocked);

        let parent_done = manager
            .set_shared_task_status(&parent.task_id, &worker.session_id, SharedTaskStatus::Done)
            .await?;
        assert_eq!(parent_done.status, SharedTaskStatus::Done);

        let tasks = manager
            .list_shared_tasks(&workflow.workflow_id, &worker.session_id, true)
            .await?;
        let child_after_parent = tasks
            .iter()
            .find(|task| task.task_id == child.task_id)
            .expect("child task should exist");
        assert_eq!(child_after_parent.status, SharedTaskStatus::Todo);

        let claimed_ready = manager
            .claim_shared_task(&child.task_id, &collaborator.session_id)
            .await?;
        assert_eq!(claimed_ready.status, SharedTaskStatus::InProgress);

        let child_done = manager
            .set_shared_task_status(
                &child.task_id,
                &collaborator.session_id,
                SharedTaskStatus::Done,
            )
            .await?;
        assert_eq!(child_done.status, SharedTaskStatus::Done);

        let open_tasks = manager
            .list_shared_tasks(&workflow.workflow_id, &worker.session_id, false)
            .await?;
        assert!(open_tasks.is_empty(), "done tasks should be filtered");

        manager.destroy_session(&worker.session_id, false).await?;
        manager
            .destroy_session(&collaborator.session_id, false)
            .await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn load_many_subscribers_receive_stream_without_blocking() -> Result<()> {
        let manager = SessionManager::new(SessionManagerConfig {
            broadcast_capacity: 1024,
            ..SessionManagerConfig::default()
        });
        let session = manager
            .create_session(sh_cmd(
                "sleep 0.2; for i in $(seq 1 200); do echo load-$i; done; sleep 0.2",
            ))
            .await?;

        let mut tasks = Vec::new();
        for _ in 0..20 {
            let sub = manager.subscribe(&session.session_id, None).await?;
            tasks.push(tokio::spawn(async move {
                let mut seen = sub
                    .replay_events
                    .iter()
                    .filter(|e| matches!(e, Event::Output { .. }))
                    .count();
                let mut receiver = sub.receiver;
                let _ = timeout(Duration::from_secs(2), async {
                    loop {
                        match receiver.recv().await {
                            Ok(Event::Output { .. }) => {
                                seen += 1;
                                if seen >= 5 {
                                    break;
                                }
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                })
                .await;
                seen
            }));
        }

        for task in tasks {
            let seen = task.await?;
            assert!(seen > 0, "subscriber should observe output under load");
        }

        manager.destroy_session(&session.session_id, false).await?;
        Ok(())
    }
}
