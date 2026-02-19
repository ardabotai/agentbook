use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum size of a JSON-lines frame on the Unix socket (64 KiB).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Typed enums for wire format safety
// ---------------------------------------------------------------------------

/// Which wallet to operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalletType {
    Human,
    Yolo,
}

impl fmt::Display for WalletType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WalletType::Human => write!(f, "human"),
            WalletType::Yolo => write!(f, "yolo"),
        }
    }
}

/// The kind of message (DM vs feed post vs room message).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    #[default]
    Unspecified,
    DmText,
    FeedPost,
    RoomMessage,
    /// Relay-generated system event: a node joined a room.
    RoomJoin,
}

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
    /// Look up a username on the relay host (username → node_id).
    LookupUsername { username: String },
    /// Reverse-lookup a node_id on the relay host (node_id → username).
    LookupNodeId { node_id: String },

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

    // -- Wallet --
    /// Get wallet info and balances.
    WalletBalance { wallet: WalletType },
    /// Send ETH on Base from human wallet. OTP required.
    SendEth {
        to: String,
        amount: String,
        otp: String,
    },
    /// Send USDC on Base from human wallet. OTP required.
    SendUsdc {
        to: String,
        amount: String,
        otp: String,
    },
    /// Send ETH from yolo wallet (no auth, agent-accessible).
    YoloSendEth { to: String, amount: String },
    /// Send USDC from yolo wallet (no auth, agent-accessible).
    YoloSendUsdc { to: String, amount: String },
    /// Set up TOTP authenticator (first time only).
    SetupTotp,
    /// Verify TOTP code (used during initial setup).
    VerifyTotp { code: String },

    // -- Smart contracts --
    /// Read a view/pure contract function. No auth needed.
    ReadContract {
        contract: String,
        abi: String,
        function: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
    },
    /// Write to a contract from human wallet. OTP required.
    WriteContract {
        contract: String,
        abi: String,
        function: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
        #[serde(default)]
        value: Option<String>,
        otp: String,
    },
    /// Write to a contract from yolo wallet. No auth.
    YoloWriteContract {
        contract: String,
        abi: String,
        function: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
        #[serde(default)]
        value: Option<String>,
    },

    // -- Message signing --
    /// EIP-191 sign a message from human wallet. OTP required.
    SignMessage { message: String, otp: String },
    /// EIP-191 sign a message from yolo wallet. No auth.
    YoloSignMessage { message: String },

    // -- Rooms --
    /// Join a room. If passphrase is provided, it becomes a secure (encrypted) room.
    JoinRoom {
        room: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        passphrase: Option<String>,
    },
    /// Leave a room.
    LeaveRoom { room: String },
    /// Send a message to a room (140-char limit, 3-second cooldown).
    SendRoom { room: String, body: String },
    /// Get messages from a specific room.
    RoomInbox {
        room: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// List all joined rooms.
    ListRooms,

    // -- Sync --
    /// Push local follow data to relay.
    SyncPush { confirm: bool },
    /// Pull follow data from relay to local store.
    SyncPull { confirm: bool },

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
        message_type: MessageType,
        preview: String,
    },
    /// A new room message arrived.
    NewRoomMessage {
        message_id: String,
        from: String,
        room: String,
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
    pub message_type: MessageType,
    pub body: String,
    pub timestamp_ms: u64,
    pub acked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
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

/// Wallet info returned by `WalletBalance`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub address: String,
    pub eth_balance: String,
    pub usdc_balance: String,
    pub wallet_type: WalletType,
}

/// Transaction result returned by send operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxResult {
    pub tx_hash: String,
    pub explorer_url: String,
}

/// TOTP setup info returned to CLI/TUI for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpSetupInfo {
    pub secret_base32: String,
    pub otpauth_url: String,
}

/// Result of a contract read operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractReadResult {
    pub result: serde_json::Value,
}

/// Result of a message signing operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureResult {
    pub signature: String,
    pub address: String,
}

/// A joined room returned by the `ListRooms` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInfo {
    pub room: String,
    pub secure: bool,
}

/// Result of a sync-push or sync-pull operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub pushed: Option<usize>,
    pub pulled: Option<usize>,
    pub added: Option<usize>,
    pub updated: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_request_serde_round_trip() {
        let requests = vec![
            Request::JoinRoom {
                room: "test-room".to_string(),
                passphrase: Some("secret".to_string()),
            },
            Request::LeaveRoom {
                room: "test-room".to_string(),
            },
            Request::SendRoom {
                room: "chat".to_string(),
                body: "hello".to_string(),
            },
            Request::RoomInbox {
                room: "chat".to_string(),
                limit: Some(50),
            },
            Request::ListRooms,
        ];

        for req in &requests {
            let json = serde_json::to_string(req).unwrap();
            let decoded: Request = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&decoded).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn room_event_serde_round_trip() {
        let event = Event::NewRoomMessage {
            message_id: "msg-1".to_string(),
            from: "node-a".to_string(),
            room: "chat".to_string(),
            preview: "hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: Event = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&decoded).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn inbox_entry_room_field_skips_none() {
        let entry = InboxEntry {
            message_id: "1".to_string(),
            from_node_id: "node-a".to_string(),
            from_username: None,
            body: "hi".to_string(),
            timestamp_ms: 1000,
            acked: false,
            message_type: MessageType::FeedPost,
            room: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("\"room\""));

        let entry_with_room = InboxEntry {
            room: Some("test".to_string()),
            ..entry
        };
        let json = serde_json::to_string(&entry_with_room).unwrap();
        assert!(json.contains("\"room\":\"test\""));
    }

    #[test]
    fn room_info_serde() {
        let info = RoomInfo {
            room: "secret".to_string(),
            secure: true,
        };
        let json = serde_json::to_string(&info).unwrap();
        let decoded: RoomInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.room, "secret");
        assert!(decoded.secure);
    }
}
