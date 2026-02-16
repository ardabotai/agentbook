use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_JSON_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_OUTPUT_CHUNK_BYTES: usize = 16 * 1024;
pub const MAX_INPUT_CHUNK_BYTES: usize = 8 * 1024;

pub type SessionId = String;
pub type AttachmentId = String;
pub type MessageId = String;
pub type TaskId = String;
pub type WorkflowId = String;
pub type WalletAddress = String;
pub type NodeId = String;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachMode {
    Edit,
    View,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxConfig {
    #[serde(default)]
    pub writable_paths: Vec<PathBuf>,
    #[serde(default)]
    pub readable_paths: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedTaskStatus {
    Todo,
    InProgress,
    Blocked,
    Done,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    Public,
    #[default]
    Follower,
    Trusted,
    Operator,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeshMessageType {
    #[default]
    Unspecified,
    DmText,
    Broadcast,
    TaskUpdate,
    Command,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentMessage {
    pub message_id: MessageId,
    pub from_session_id: Option<SessionId>,
    pub to_session_id: SessionId,
    pub topic: Option<String>,
    #[serde(default)]
    pub encrypted: bool,
    pub body: String,
    #[serde(default)]
    pub ciphertext_b64: Option<String>,
    #[serde(default)]
    pub nonce_b64: Option<String>,
    #[serde(default)]
    pub signature_b64: Option<String>,
    #[serde(default)]
    pub signer_session_id: Option<SessionId>,
    #[serde(default)]
    pub signer_public_key_b64: Option<String>,
    #[serde(default)]
    pub signature_valid: Option<bool>,
    pub requires_response: bool,
    pub created_at_ms: u128,
    pub read_at_ms: Option<u128>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentWalletPublic {
    pub session_id: SessionId,
    pub address: WalletAddress,
    pub encryption_public_key_b64: String,
    pub signing_public_key_b64: String,
    pub created_at_ms: u128,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SharedTask {
    pub task_id: TaskId,
    pub workflow_id: WorkflowId,
    pub title: String,
    pub description: Option<String>,
    pub status: SharedTaskStatus,
    pub created_by: Option<SessionId>,
    pub assignee_session_id: Option<SessionId>,
    pub depends_on: Vec<TaskId>,
    pub created_at_ms: u128,
    pub updated_at_ms: u128,
    pub completed_at_ms: Option<u128>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WorkflowMember {
    pub session_id: SessionId,
    pub parent_session_id: Option<SessionId>,
    pub joined_at_ms: u128,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrchestrationWorkflow {
    pub workflow_id: WorkflowId,
    pub name: String,
    pub root_session_id: SessionId,
    pub members: Vec<WorkflowMember>,
    pub created_at_ms: u128,
    pub updated_at_ms: u128,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FriendRecord {
    pub node_id: NodeId,
    pub public_key_b64: String,
    pub alias: Option<String>,
    #[serde(default)]
    pub relay_hosts: Vec<String>,
    #[serde(default)]
    pub blocked: bool,
    pub added_at_ms: u64,
    #[serde(default)]
    pub trust_tier: TrustTier,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NodeInboxMessage {
    pub message_id: String,
    pub from_node_id: NodeId,
    pub from_public_key_b64: String,
    pub topic: Option<String>,
    pub body: String,
    pub timestamp_ms: u64,
    #[serde(default)]
    pub acked: bool,
    #[serde(default)]
    pub message_type: MeshMessageType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NodeInfoResponse {
    pub node_id: NodeId,
    pub public_key_b64: String,
    pub started_at_ms: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NodeSendRemoteResponse {
    pub message_id: String,
    pub delivered: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InviteAcceptResponse {
    pub inviter_node_id: NodeId,
    pub inviter_public_key_b64: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    NotFound,
    PermissionDenied,
    Conflict,
    Internal,
    Unsupported,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    // --- Session management ---
    SessionCreate {
        exec: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        tags: Vec<String>,
        cwd: Option<PathBuf>,
        label: Option<String>,
        sandbox: Option<SandboxConfig>,
        parent_id: Option<SessionId>,
        cols: u16,
        rows: u16,
    },
    SessionDestroy {
        session_id: SessionId,
        #[serde(default)]
        cascade: bool,
    },
    SessionList,
    SessionTree,
    SessionInfo {
        session_id: SessionId,
    },
    Attach {
        session_id: SessionId,
        mode: AttachMode,
        last_seq_seen: Option<u64>,
    },
    Detach {
        attachment_id: AttachmentId,
    },
    SendInput {
        session_id: SessionId,
        attachment_id: AttachmentId,
        data_b64: String,
    },
    Resize {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },
    MarkerInsert {
        session_id: SessionId,
        name: String,
    },
    MarkerList {
        session_id: SessionId,
    },

    // --- Inter-agent messaging ---
    MessageSend {
        from_session_id: Option<SessionId>,
        to_session_id: SessionId,
        topic: Option<String>,
        body: String,
        #[serde(default)]
        requires_response: bool,
        #[serde(default)]
        encrypt: bool,
        #[serde(default)]
        sign: bool,
    },
    MessageList {
        session_id: SessionId,
        #[serde(default)]
        unread_only: bool,
        limit: Option<usize>,
    },
    MessageAck {
        session_id: SessionId,
        message_id: MessageId,
    },
    MessageUnreadCount {
        session_id: SessionId,
    },

    // --- Workflows & shared tasks ---
    WorkflowCreate {
        name: String,
        root_session_id: SessionId,
    },
    WorkflowJoin {
        workflow_id: WorkflowId,
        session_id: SessionId,
        parent_session_id: SessionId,
    },
    WorkflowLeave {
        workflow_id: WorkflowId,
        session_id: SessionId,
    },
    WorkflowList {
        session_id: SessionId,
    },
    TaskCreate {
        workflow_id: WorkflowId,
        title: String,
        description: Option<String>,
        created_by: SessionId,
        #[serde(default)]
        depends_on: Vec<TaskId>,
    },
    TaskList {
        workflow_id: WorkflowId,
        session_id: SessionId,
        #[serde(default)]
        include_done: bool,
    },
    TaskClaim {
        task_id: TaskId,
        session_id: SessionId,
    },
    TaskSetStatus {
        task_id: TaskId,
        session_id: SessionId,
        status: SharedTaskStatus,
    },
    WalletInfo {
        session_id: SessionId,
    },

    // --- Subscriptions & lifecycle ---
    Subscribe {
        session_id: SessionId,
        last_seq_seen: Option<u64>,
    },
    Unsubscribe {
        session_id: SessionId,
    },
    ServerShutdown,
    Health,

    // --- Mesh: node identity ---
    NodeInfo,

    // --- Mesh: invites ---
    InviteCreate {
        #[serde(default)]
        relay_hosts: Vec<String>,
        #[serde(default)]
        scopes: Vec<String>,
        ttl_ms: u64,
    },
    InviteAccept {
        token: String,
    },

    // --- Mesh: friends ---
    FriendsList,
    FriendsBlock {
        node_id: NodeId,
    },
    FriendsUnblock {
        node_id: NodeId,
    },
    FriendsRemove {
        node_id: NodeId,
    },
    FriendsSetTrust {
        node_id: NodeId,
        trust_tier: TrustTier,
    },

    // --- Mesh: node inbox ---
    NodeInboxList {
        #[serde(default)]
        unread_only: bool,
        limit: Option<usize>,
    },
    NodeInboxAck {
        message_id: String,
    },

    // --- Mesh: remote messaging ---
    NodeSendRemote {
        to_node_id: NodeId,
        topic: Option<String>,
        body: String,
        #[serde(default)]
        encrypt: bool,
        invite_token: Option<String>,
        #[serde(default)]
        message_type: MeshMessageType,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Output {
        session_id: SessionId,
        seq: u64,
        data_b64: String,
    },
    Snapshot {
        session_id: SessionId,
        seq: u64,
        cols: u16,
        rows: u16,
        lines: Vec<String>,
    },
    SessionCreated {
        session_id: SessionId,
        label: Option<String>,
    },
    SessionExited {
        session_id: SessionId,
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    SessionDestroyed {
        session_id: SessionId,
    },
    MarkerInserted {
        session_id: SessionId,
        name: String,
        seq: u64,
    },
    MessageReceived {
        session_id: SessionId,
        message: AgentMessage,
    },
    WorkflowUpdated {
        workflow: OrchestrationWorkflow,
    },
    TaskUpdated {
        task: SharedTask,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol_version: u32,
        features: Vec<String>,
    },
    Ok {
        data: Option<serde_json::Value>,
    },
    Error {
        message: String,
        code: ErrorCode,
    },
    Event {
        event: Box<Event>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub label: Option<String>,
    pub tags: Vec<String>,
    pub exec: String,
    pub cwd: PathBuf,
    pub parent_id: Option<SessionId>,
    pub created_at_ms: u128,
    pub sandboxed: bool,
    pub git_branch: Option<String>,
    pub git_repo_root: Option<PathBuf>,
    pub git_worktree_path: Option<PathBuf>,
    pub git_dirty: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AttachmentInfo {
    pub attachment_id: AttachmentId,
    pub session_id: SessionId,
    pub mode: AttachMode,
    pub created_at_ms: u128,
}

impl Response {
    pub fn ok(data: Option<serde_json::Value>) -> Self {
        Self::Ok { data }
    }

    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            code,
        }
    }

    pub fn hello(features: Vec<String>) -> Self {
        Self::Hello {
            protocol_version: PROTOCOL_VERSION,
            features,
        }
    }
}

/// Returns true if the request requires mesh networking support (tmax-node).
/// tmax-local should return Unsupported for these.
impl Request {
    pub fn is_mesh(&self) -> bool {
        matches!(
            self,
            Request::NodeInfo
                | Request::InviteCreate { .. }
                | Request::InviteAccept { .. }
                | Request::FriendsList
                | Request::FriendsBlock { .. }
                | Request::FriendsUnblock { .. }
                | Request::FriendsRemove { .. }
                | Request::FriendsSetTrust { .. }
                | Request::NodeInboxList { .. }
                | Request::NodeInboxAck { .. }
                | Request::NodeSendRemote { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn request_round_trip() -> Result<()> {
        let req = Request::Attach {
            session_id: "s1".to_string(),
            mode: AttachMode::Edit,
            last_seq_seen: Some(42),
        };
        let encoded = serde_json::to_string(&req)?;
        let decoded: Request = serde_json::from_str(&encoded)?;
        match decoded {
            Request::Attach {
                session_id,
                mode,
                last_seq_seen,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(mode, AttachMode::Edit);
                assert_eq!(last_seq_seen, Some(42));
            }
            _ => panic!("unexpected variant"),
        }
        Ok(())
    }

    #[test]
    fn response_round_trip() -> Result<()> {
        let resp = Response::Event {
            event: Box::new(Event::Output {
                session_id: "s1".to_string(),
                seq: 7,
                data_b64: "SGVsbG8=".to_string(),
            }),
        };
        let encoded = serde_json::to_string(&resp)?;
        let decoded: Response = serde_json::from_str(&encoded)?;
        match decoded {
            Response::Event { event } => match *event {
                Event::Output {
                    session_id,
                    seq,
                    data_b64,
                } => {
                    assert_eq!(session_id, "s1");
                    assert_eq!(seq, 7);
                    assert_eq!(data_b64, "SGVsbG8=");
                }
                _ => panic!("unexpected event variant"),
            },
            _ => panic!("unexpected variant"),
        }
        Ok(())
    }

    #[test]
    fn mesh_request_round_trips() -> Result<()> {
        let cases: Vec<Request> = vec![
            Request::NodeInfo,
            Request::InviteCreate {
                relay_hosts: vec!["relay.example.com:50060".into()],
                scopes: vec![],
                ttl_ms: 3_600_000,
            },
            Request::InviteAccept {
                token: "tok_abc".into(),
            },
            Request::FriendsList,
            Request::FriendsBlock {
                node_id: "n1".into(),
            },
            Request::FriendsUnblock {
                node_id: "n1".into(),
            },
            Request::FriendsRemove {
                node_id: "n1".into(),
            },
            Request::FriendsSetTrust {
                node_id: "n1".into(),
                trust_tier: TrustTier::Trusted,
            },
            Request::NodeInboxList {
                unread_only: true,
                limit: Some(50),
            },
            Request::NodeInboxAck {
                message_id: "msg1".into(),
            },
            Request::NodeSendRemote {
                to_node_id: "n2".into(),
                topic: Some("greeting".into()),
                body: "hello".into(),
                encrypt: true,
                invite_token: None,
                message_type: MeshMessageType::DmText,
            },
            Request::Health,
        ];

        for req in cases {
            let encoded = serde_json::to_string(&req)?;
            let decoded: Request = serde_json::from_str(&encoded)?;
            // Verify round-trip doesn't panic and produces valid JSON
            let re_encoded = serde_json::to_string(&decoded)?;
            assert_eq!(encoded, re_encoded);
        }
        Ok(())
    }

    #[test]
    fn mesh_types_round_trip() -> Result<()> {
        let friend = FriendRecord {
            node_id: "n1".into(),
            public_key_b64: "abc123".into(),
            alias: Some("alice".into()),
            relay_hosts: vec!["relay.example.com".into()],
            blocked: false,
            added_at_ms: 1000,
            trust_tier: TrustTier::Trusted,
        };
        let encoded = serde_json::to_string(&friend)?;
        let decoded: FriendRecord = serde_json::from_str(&encoded)?;
        assert_eq!(decoded.node_id, "n1");
        assert_eq!(decoded.trust_tier, TrustTier::Trusted);

        let inbox_msg = NodeInboxMessage {
            message_id: "m1".into(),
            from_node_id: "n2".into(),
            from_public_key_b64: "key".into(),
            topic: None,
            body: "hello".into(),
            timestamp_ms: 2000,
            acked: false,
            message_type: MeshMessageType::DmText,
        };
        let encoded = serde_json::to_string(&inbox_msg)?;
        let decoded: NodeInboxMessage = serde_json::from_str(&encoded)?;
        assert_eq!(decoded.message_type, MeshMessageType::DmText);

        Ok(())
    }

    #[test]
    fn is_mesh_classification() {
        assert!(!Request::SessionList.is_mesh());
        assert!(!Request::Health.is_mesh());
        assert!(Request::NodeInfo.is_mesh());
        assert!(Request::FriendsList.is_mesh());
        assert!(
            Request::NodeInboxList {
                unread_only: false,
                limit: None
            }
            .is_mesh()
        );
    }
}
