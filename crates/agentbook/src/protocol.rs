use serde::{Deserialize, Serialize};

/// Maximum size of a JSON-lines frame on the Unix socket (64 KiB).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// A request sent from CLI/TUI to the node daemon over the Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Get node identity info.
    Identity,
    /// Get health status.
    Health,

    // -- Follow graph --
    /// Follow a node by node_id or @username.
    Follow { target: String },
    /// Unfollow a node.
    Unfollow { target: String },
    /// Block a node.
    Block { target: String },
    /// List nodes we follow.
    Following,
    /// List nodes that follow us (known followers).
    Followers,

    // -- Username directory --
    /// Register a username on the relay host.
    RegisterUsername { username: String },
    /// Look up a username on the relay host.
    LookupUsername { username: String },

    // -- Messaging --
    /// Send a DM to a mutual follow.
    SendDm { to: String, body: String },
    /// Post to feed (encrypted per-follower).
    PostFeed { body: String },
    /// List inbox messages.
    Inbox {
        #[serde(default)]
        unread_only: bool,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Acknowledge (mark as read) a message.
    InboxAck { message_id: String },

    // -- Daemon lifecycle --
    /// Shut down the daemon.
    Shutdown,
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// A response sent from the node daemon to CLI/TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Connection established.
    Hello { node_id: String, version: String },
    /// Request succeeded with optional data.
    Ok { data: Option<serde_json::Value> },
    /// Request failed.
    Error { code: String, message: String },
    /// Asynchronous event (new message, etc.).
    Event { event: Event },
}

// ---------------------------------------------------------------------------
// Event
// ---------------------------------------------------------------------------

/// Asynchronous events pushed to connected clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A new message arrived in the inbox.
    NewMessage {
        message_id: String,
        from: String,
        message_type: String,
        preview: String,
    },
    /// A new follower detected.
    NewFollower { node_id: String },
}

// ---------------------------------------------------------------------------
// Data types returned in Ok.data
// ---------------------------------------------------------------------------

/// Identity info returned by the `Identity` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityInfo {
    pub node_id: String,
    pub public_key_b64: String,
    pub username: Option<String>,
}

/// A follow record returned in the following/followers list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowInfo {
    pub node_id: String,
    pub username: Option<String>,
    pub followed_at_ms: u64,
}

/// A message record returned by the `Inbox` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxEntry {
    pub message_id: String,
    pub from_node_id: String,
    pub from_username: Option<String>,
    pub message_type: String,
    pub body: String,
    pub timestamp_ms: u64,
    pub acked: bool,
}

/// Username lookup result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsernameLookup {
    pub username: String,
    pub node_id: String,
    pub public_key_b64: String,
}

/// Health status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub healthy: bool,
    pub relay_connected: bool,
    pub following_count: usize,
    pub unread_count: usize,
}
