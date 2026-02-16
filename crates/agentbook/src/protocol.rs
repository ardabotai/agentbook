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

    // -- Wallet --
    /// Get wallet info and balances. `wallet` selects which wallet: "human" or "yolo".
    WalletBalance { wallet: String },
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

/// Wallet info returned by `WalletBalance`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub address: String,
    pub eth_balance: String,
    pub usdc_balance: String,
    pub wallet_type: String,
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
