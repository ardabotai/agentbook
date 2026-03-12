use agentbook::protocol::{Event, InboxEntry, MessageType};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

/// Maximum number of chat history entries to keep in memory.
pub const MAX_CHAT_HISTORY: usize = 200;

/// Tracks what kind of response we're expecting from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingRequest {
    Inbox,
    Following,
    Send,
    ListRooms,
    RoomInbox(String),
    Identity,
    InboxAck,
    // Slash commands that return data to display in status bar
    SlashIdentity,
    SlashHealth,
    SlashBalance,
    SlashLookup,
    SlashFollowers,
    SlashFollowing,
}

/// Number of lines scrolled per mouse wheel tick.
pub const SCROLL_STEP: usize = 3;
const TUI_PREFS_FILE: &str = "agentbook_tui_prefs.json";

/// Which tab is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tab {
    Feed,
    Dms,
    Terminal,
    Room(String),
}

/// Terminal pane layout direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSplit {
    /// One pane only.
    Single,
    /// Left/right panes.
    Vertical,
    /// Top/bottom panes.
    Horizontal,
}

/// Terminal auto-agent mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoAgentMode {
    /// Local safe heuristics for auto-advance prompts.
    Rules,
    /// External PI-backed command integration.
    Pi,
}

impl AutoAgentMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rules => "RULES",
            Self::Pi => "PI",
        }
    }
}

/// Runtime state for terminal auto-agent behavior.
pub struct AutoAgentState {
    pub enabled: bool,
    pub mode: AutoAgentMode,
    pub interval_secs: u64,
    pub last_tick_at: Option<Instant>,
    pub last_action_at: Option<Instant>,
    pub last_summary: String,
    pub awaiting_api_key: bool,
    pub auth_error: Option<String>,
    pub awaiting_user_input: bool,
    pub pending_user_question: Option<String>,
    pub chat_focus: bool,
    pub chat_input: String,
    pub chat_history: Vec<SidekickMessage>,
    pub chat_scroll: usize,
    pub chat_streaming: bool,
    pub chat_stream_rx: Option<mpsc::Receiver<SidekickChatStreamEvent>>,
    /// Cancellation flag for the streaming reader thread. Set to `true` to
    /// signal the thread to stop reading and kill the child process.
    pub stream_cancel: Arc<AtomicBool>,
    pub chat_queue: VecDeque<String>,
    /// Cached result of `has_arda_login()` to avoid filesystem I/O in render path.
    pub cached_has_arda: bool,
    /// Environment variables to pass to child inference processes.
    pub inference_env: Vec<(String, String)>,
    /// Last time we polled for Arda login status (for auto-poll in awaiting_api_key state).
    pub last_arda_check: Option<Instant>,
    /// Last time `load_inference_env_vars()` was called (for TTL-based caching).
    pub last_env_load: Option<Instant>,
    /// True while the Arda OAuth login flow is running in a background thread.
    pub login_in_progress: bool,
    /// When the Arda login flow was started (for timeout detection).
    pub login_started_at: Option<Instant>,
}

impl AutoAgentState {
    /// Reset the auto-agent state back to idle (e.g. when disabling, clearing, or toggling off).
    pub fn reset(&mut self) {
        self.chat_focus = false;
        self.awaiting_api_key = false;
        self.awaiting_user_input = false;
        self.pending_user_question = None;
        self.chat_scroll = 0;
        self.chat_streaming = false;
        self.stream_cancel.store(true, Ordering::Relaxed);
        self.stream_cancel = Arc::new(AtomicBool::new(false));
        self.chat_stream_rx = None;
        self.chat_queue.clear();
        self.login_in_progress = false;
        self.login_started_at = None;
    }

    /// Push a message to chat history, capping at [`MAX_CHAT_HISTORY`].
    pub fn push_chat(&mut self, msg: SidekickMessage) {
        self.chat_history.push(msg);
        if self.chat_history.len() > MAX_CHAT_HISTORY {
            let excess = self.chat_history.len() - MAX_CHAT_HISTORY;
            self.chat_history.drain(..excess);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidekickRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidekickMessage {
    pub role: SidekickRole,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct SidekickChatCompletion {
    pub target_window: Option<usize>,
    pub keys: Option<String>,
    pub action_note: Option<String>,
    pub summary: String,
    pub reply: Option<String>,
    pub requires_api_key: Option<String>,
    pub requires_user_input: bool,
    pub user_question: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SidekickChatStreamEvent {
    ReplyDelta(String),
    Complete(SidekickChatCompletion),
    Error(String),
}

/// The TUI application state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixPending {
    TerminalTabSelect,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct PersistedPreferences {
    notification_sound_enabled: bool,
    sidekick_enabled: bool,
}

/// The TUI application state.
pub struct App {
    pub tab: Tab,
    pub input: String,
    pub messages: Vec<InboxEntry>,
    pub following: Vec<String>,
    pub selected_contact: usize,
    pub node_id: String,
    pub status_msg: String,
    pub should_quit: bool,
    pub quit_confirm: bool,
    pub request_full_redraw: bool,
    pub auto_agent: AutoAgentState,

    /// Prefix-mode keybinding state (Ctrl+Space leader).
    pub prefix_mode: bool,
    pub prefix_mode_at: Option<std::time::Instant>,
    pub prefix_pending: Option<PrefixPending>,
    /// Audible notification toggle for important tab events.
    pub notification_sound_enabled: bool,

    /// Per-tab unread activity indicators.
    pub activity_feed: bool,
    pub activity_dms: bool,
    pub activity_terminal: bool,

    /// Embedded terminal panes.
    pub terminals: Vec<crate::terminal::TerminalEmulator>,
    /// Active terminal pane index.
    pub active_terminal: usize,
    /// Split layout for multiple panes.
    pub terminal_split: TerminalSplit,
    /// Terminal tabs (tmux windows) shown in top bar.
    pub terminal_window_tabs: Vec<String>,
    /// tmux window indices corresponding to `terminal_window_tabs`.
    pub terminal_window_indices: Vec<usize>,
    /// Active terminal window tab index.
    pub active_terminal_window: usize,
    /// tmux window indices currently waiting for user input (prompt detected).
    pub terminal_waiting_input_windows: HashSet<usize>,

    /// Joined rooms, ordered (determines tab order).
    pub rooms: Vec<String>,
    /// Per-room message buffers.
    pub room_messages: HashMap<String, Vec<InboxEntry>>,
    /// Per-room unread activity indicators.
    pub activity_rooms: HashMap<String, bool>,
    /// Which rooms are secure (for lock icon).
    pub secure_rooms: HashSet<String>,
    /// Blocked node IDs (for client-side filtering).
    pub blocked_nodes: HashSet<String>,

    /// Terminal scroll mode (tmux-style `[` chord).
    pub scroll_mode: bool,

    /// Per-tab scroll offsets (0 = pinned to bottom/latest).
    /// Key: "feed", "dms:{node_id}", "room:{name}"
    pub scroll: HashMap<String, usize>,

    /// Username registered on the relay (fetched at startup).
    pub username: Option<String>,

    /// Message IDs we've already sent InboxAck for (avoid duplicate acks).
    pub acked_ids: HashSet<String>,

    /// Terminal tab rename mode: Some(buffer) when actively renaming.
    pub rename_input: Option<String>,
}

/// Char-safe truncation: truncates `s` to at most `max` characters, appending
/// an ellipsis if truncated.  Avoids byte-based slicing that can panic on
/// multi-byte UTF-8.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "\u{2026}"
}

impl App {
    pub fn new(node_id: String) -> Self {
        Self {
            tab: Tab::Terminal,
            input: String::new(),
            messages: Vec::new(),
            following: Vec::new(),
            selected_contact: 0,
            node_id,
            status_msg: String::new(),
            should_quit: false,
            quit_confirm: false,
            request_full_redraw: true,
            auto_agent: AutoAgentState {
                enabled: false,
                mode: AutoAgentMode::Rules,
                interval_secs: 6,
                last_tick_at: None,
                last_action_at: None,
                last_summary: String::new(),
                awaiting_api_key: false,
                auth_error: None,
                awaiting_user_input: false,
                pending_user_question: None,
                chat_focus: false,
                chat_input: String::new(),
                chat_history: Vec::new(),
                chat_scroll: 0,
                chat_streaming: false,
                chat_stream_rx: None,
                stream_cancel: Arc::new(AtomicBool::new(false)),
                chat_queue: VecDeque::new(),
                cached_has_arda: false,
                inference_env: Vec::new(),
                last_arda_check: None,
                last_env_load: None,
                login_in_progress: false,
                login_started_at: None,
            },
            prefix_mode: false,
            prefix_mode_at: None,
            prefix_pending: None,
            notification_sound_enabled: notification_sound_default(),
            activity_feed: false,
            activity_dms: false,
            activity_terminal: false,
            terminals: Vec::new(),
            active_terminal: 0,
            terminal_split: TerminalSplit::Single,
            terminal_window_tabs: Vec::new(),
            terminal_window_indices: Vec::new(),
            active_terminal_window: 0,
            terminal_waiting_input_windows: HashSet::new(),
            rooms: Vec::new(),
            room_messages: HashMap::new(),
            activity_rooms: HashMap::new(),
            secure_rooms: HashSet::new(),
            blocked_nodes: HashSet::new(),
            scroll_mode: false,
            scroll: HashMap::new(),
            username: None,
            acked_ids: HashSet::new(),
            rename_input: None,
        }
    }

    /// All tabs in display order: Terminal, Feed, DMs, then rooms.
    #[allow(dead_code)]
    pub fn all_tabs(&self) -> Vec<Tab> {
        let mut tabs = vec![Tab::Terminal, Tab::Feed, Tab::Dms];
        for room in &self.rooms {
            tabs.push(Tab::Room(room.clone()));
        }
        tabs
    }

    /// Index of the current tab in the all_tabs list.
    #[allow(dead_code)]
    pub fn tab_index(&self) -> usize {
        self.all_tabs()
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0)
    }

    /// Switch to a tab, clearing its activity indicator.
    pub fn switch_tab(&mut self, tab: Tab) {
        self.scroll_mode = false;
        match &tab {
            Tab::Feed => self.activity_feed = false,
            Tab::Dms => self.activity_dms = false,
            Tab::Terminal => self.activity_terminal = false,
            Tab::Room(room) => {
                self.activity_rooms.insert(room.clone(), false);
            }
        }
        self.tab = tab;
        self.request_full_redraw = true;
    }

    pub fn clamp_selected_contact(&mut self) {
        if self.following.is_empty() {
            self.selected_contact = 0;
            return;
        }
        self.selected_contact = self.selected_contact.min(self.following.len() - 1);
    }

    pub fn selected_contact_node_id(&self) -> Option<&str> {
        self.following
            .get(self.selected_contact)
            .map(|s| s.as_str())
    }

    fn dm_peer_node_id<'a>(&'a self, entry: &'a InboxEntry) -> Option<&'a str> {
        if entry.message_type != MessageType::DmText {
            return None;
        }
        if entry.from_node_id == self.node_id {
            entry.to_node_id.as_deref()
        } else {
            Some(entry.from_node_id.as_str())
        }
    }

    /// Handle an event pushed from the node daemon.
    /// Returns `true` when the event created new off-tab activity that should
    /// trigger the notification sound.
    pub fn handle_event(&mut self, event: Event) -> bool {
        let mut notify = false;
        match event {
            Event::NewMessage {
                message_type, from, ..
            } => match message_type {
                MessageType::FeedPost => {
                    if self.tab != Tab::Feed {
                        notify |= !self.activity_feed;
                        self.activity_feed = true;
                    }
                }
                MessageType::DmText => {
                    let selected_contact = self.selected_contact_node_id();
                    if self.tab != Tab::Dms || selected_contact != Some(from.as_str()) {
                        notify |= !self.activity_dms;
                        self.activity_dms = true;
                        if self.tab == Tab::Dms {
                            self.status_msg = format!("New DM from {}", truncate(&from, 16));
                        }
                    }
                }
                MessageType::Unspecified | MessageType::RoomMessage | MessageType::RoomJoin => {}
            },
            Event::NewRoomMessage { room, .. } => {
                if self.tab != Tab::Room(room.clone()) {
                    notify |= !self.activity_rooms.get(&room).copied().unwrap_or(false);
                    self.activity_rooms.insert(room, true);
                }
            }
            Event::NewFollower { .. } => {}
        }
        notify
    }

    /// Scroll key for the current tab view.
    fn scroll_key(&self) -> String {
        match &self.tab {
            Tab::Feed => "feed".to_string(),
            Tab::Dms => format!(
                "dms:{}",
                self.following
                    .get(self.selected_contact)
                    .map(|s| s.as_str())
                    .unwrap_or("")
            ),
            Tab::Terminal => "terminal".to_string(),
            Tab::Room(room) => format!("room:{room}"),
        }
    }

    /// Current scroll offset for the active tab (0 = pinned to bottom).
    pub fn current_scroll(&self) -> usize {
        self.scroll.get(&self.scroll_key()).copied().unwrap_or(0)
    }

    /// Scroll up (toward older messages).
    pub fn scroll_up(&mut self) {
        let key = self.scroll_key();
        *self.scroll.entry(key).or_insert(0) += SCROLL_STEP;
        self.request_full_redraw = true;
    }

    /// Scroll down (toward newer messages). Clamps at 0.
    pub fn scroll_down(&mut self) {
        let key = self.scroll_key();
        let entry = self.scroll.entry(key).or_insert(0);
        *entry = entry.saturating_sub(SCROLL_STEP);
        self.request_full_redraw = true;
    }

    /// Get messages filtered for the current tab.
    pub fn visible_messages(&self) -> Vec<&InboxEntry> {
        match &self.tab {
            Tab::Feed => self
                .messages
                .iter()
                .filter(|m| m.message_type == MessageType::FeedPost)
                .collect(),
            Tab::Dms => {
                let contact = self.selected_contact_node_id();
                self.messages
                    .iter()
                    .filter(|m| contact.is_some_and(|c| self.dm_peer_node_id(m) == Some(c)))
                    .collect()
            }
            Tab::Terminal => Vec::new(),
            Tab::Room(room) => self
                .room_messages
                .get(room)
                .map(|msgs| {
                    msgs.iter()
                        .filter(|m| !self.blocked_nodes.contains(&m.from_node_id))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Mutable reference to the active terminal pane, if any.
    pub fn active_terminal_mut(&mut self) -> Option<&mut crate::terminal::TerminalEmulator> {
        self.terminals.get_mut(self.active_terminal)
    }

    /// Immutable reference to the active terminal pane, if any.
    pub fn active_terminal(&self) -> Option<&crate::terminal::TerminalEmulator> {
        self.terminals.get(self.active_terminal)
    }

    /// Refresh terminal tabs from mux backend (tmux windows), or fallback to
    /// a single local shell tab.
    pub fn refresh_terminal_tabs(&mut self) {
        self.request_full_redraw = true;
        if self.terminals.is_empty() {
            self.terminal_window_tabs.clear();
            self.terminal_window_indices.clear();
            self.terminal_waiting_input_windows.clear();
            self.active_terminal_window = 0;
            return;
        }
        let windows = if let Some(term) = self.active_terminal_mut() {
            term.mux_windows()
        } else {
            Ok(None)
        };
        match windows {
            Ok(Some(ws)) if !ws.is_empty() => {
                self.terminal_window_tabs = ws
                    .iter()
                    .map(|w| {
                        let label = terminal_tab_label(&w.name, w.pane_path.as_deref());
                        format!("{} {label}", w.index + 1)
                    })
                    .collect();
                self.terminal_window_indices = ws.iter().map(|w| w.index).collect();
                self.active_terminal_window = ws.iter().position(|w| w.active).unwrap_or(0);
                self.terminal_waiting_input_windows
                    .retain(|idx| self.terminal_window_indices.contains(idx));
            }
            Ok(_) => {
                self.terminal_window_tabs = vec!["1 shell".to_string()];
                self.terminal_window_indices = vec![0];
                self.active_terminal_window = 0;
                self.terminal_waiting_input_windows.retain(|idx| *idx == 0);
            }
            Err(e) => {
                self.status_msg = format!("tmux tab refresh failed: {e}");
            }
        }
    }

    pub fn load_preferences(&mut self) -> Result<()> {
        let path = preferences_path()?;
        self.load_preferences_from_path(&path)
    }

    pub fn persist_preferences(&self) -> Result<()> {
        let path = preferences_path()?;
        self.persist_preferences_to_path(&path)
    }

    fn load_preferences_from_path(&mut self, path: &Path) -> Result<()> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let prefs: PersistedPreferences = serde_json::from_str(&raw)
            .with_context(|| format!("invalid preferences JSON in {}", path.display()))?;
        self.notification_sound_enabled = prefs.notification_sound_enabled;
        self.auto_agent.enabled = prefs.sidekick_enabled;
        Ok(())
    }

    fn persist_preferences_to_path(&self, path: &Path) -> Result<()> {
        let prefs = PersistedPreferences {
            notification_sound_enabled: self.notification_sound_enabled,
            sidekick_enabled: self.auto_agent.enabled,
        };
        if let Some(parent) = path.parent() {
            agentbook_mesh::state_dir::ensure_state_dir(parent)?;
        }
        let payload = serde_json::to_vec_pretty(&prefs).context("failed to encode preferences")?;

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("failed to open {}", path.display()))?;
            f.write_all(&payload)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }

        #[cfg(not(unix))]
        {
            fs::write(path, payload)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }

        Ok(())
    }
}

/// Default shell names that indicate the tab hasn't been user-renamed.
const DEFAULT_SHELL_NAMES: &[&str] = &["zsh", "bash", "fish", "sh", "nu", "pwsh", "powershell"];

/// Build a human-friendly tab label. If the window name is a default shell
/// name, show the last directory component instead.
fn terminal_tab_label(window_name: &str, pane_path: Option<&str>) -> String {
    let is_default = DEFAULT_SHELL_NAMES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(window_name));
    if is_default && let Some(path) = pane_path {
        let dir = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path);
        return dir.to_string();
    }
    window_name.to_string()
}

fn preferences_path() -> Result<PathBuf> {
    let state_dir = agentbook_mesh::state_dir::default_state_dir()
        .context("failed to locate state directory")?;
    Ok(state_dir.join(TUI_PREFS_FILE))
}

fn notification_sound_default() -> bool {
    std::env::var("AGENTBOOK_NOTIFICATION_SOUND")
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn make_entry(from: &str, body: &str, msg_type: MessageType) -> InboxEntry {
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        InboxEntry {
            message_id: format!("msg-{id}"),
            from_node_id: from.to_string(),
            from_username: None,
            to_node_id: None,
            body: body.to_string(),
            timestamp_ms: 0,
            acked: false,
            message_type: msg_type,
            room: None,
        }
    }

    // ── visible_messages ─────────────────────────────────────────────────────

    #[test]
    fn visible_messages_feed_shows_only_feed_posts() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        app.messages = vec![
            make_entry("a", "post", MessageType::FeedPost),
            make_entry("b", "dm", MessageType::DmText),
        ];
        let visible = app.visible_messages();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].body, "post");
    }

    #[test]
    fn preferences_round_trip_persists_sidekick_and_sound() {
        let base = std::env::temp_dir().join(format!(
            "agentbook-tui-prefs-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        let _ = fs::create_dir_all(&base);
        let path = base.join("prefs.json");

        let mut app = App::new("me".to_string());
        app.notification_sound_enabled = true;
        app.auto_agent.enabled = true;
        app.persist_preferences_to_path(&path)
            .expect("persist should succeed");

        let mut loaded = App::new("other".to_string());
        loaded
            .load_preferences_from_path(&path)
            .expect("load should succeed");
        assert!(loaded.notification_sound_enabled);
        assert!(loaded.auto_agent.enabled);

        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn load_preferences_missing_file_is_noop() {
        let base = std::env::temp_dir().join(format!(
            "agentbook-tui-prefs-missing-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        let path = base.join("missing.json");

        let mut app = App::new("me".to_string());
        app.notification_sound_enabled = false;
        app.auto_agent.enabled = false;
        app.load_preferences_from_path(&path)
            .expect("missing file should not fail");
        assert!(!app.notification_sound_enabled);
        assert!(!app.auto_agent.enabled);
    }

    #[test]
    fn visible_messages_dms_filters_by_selected_contact() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Dms;
        app.following = vec!["alice".to_string(), "bob".to_string()];
        app.selected_contact = 0; // alice
        let mut outgoing = make_entry("me", "reply", MessageType::DmText);
        outgoing.to_node_id = Some("alice".to_string());
        app.messages = vec![make_entry("alice", "hi", MessageType::DmText), outgoing];
        let visible = app.visible_messages();
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().any(|m| m.from_node_id == "alice"));
        assert!(
            visible
                .iter()
                .any(|m| m.to_node_id.as_deref() == Some("alice"))
        );
    }

    #[test]
    fn visible_messages_dms_empty_without_selected_contact() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Dms;
        app.messages = vec![
            make_entry("x", "dm1", MessageType::DmText),
            make_entry("y", "dm2", MessageType::DmText),
        ];
        let visible = app.visible_messages();
        assert!(visible.is_empty());
    }

    #[test]
    fn visible_messages_room_shows_room_messages() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Room("general".to_string());
        app.room_messages.insert(
            "general".to_string(),
            vec![
                make_entry("a", "hello", MessageType::RoomMessage),
                make_entry("b", "world", MessageType::RoomMessage),
            ],
        );
        let visible = app.visible_messages();
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn visible_messages_room_filters_blocked_users() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Room("chat".to_string());
        app.blocked_nodes.insert("spammer".to_string());
        app.room_messages.insert(
            "chat".to_string(),
            vec![
                make_entry("alice", "good", MessageType::RoomMessage),
                make_entry("spammer", "bad", MessageType::RoomMessage),
            ],
        );
        let visible = app.visible_messages();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].from_node_id, "alice");
    }

    #[test]
    fn visible_messages_terminal_is_empty() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Terminal;
        app.messages = vec![make_entry("a", "x", MessageType::FeedPost)];
        assert!(app.visible_messages().is_empty());
    }

    // ── Tab switching & activity ──────────────────────────────────────────────

    #[test]
    fn switch_tab_clears_feed_activity() {
        let mut app = App::new("me".to_string());
        app.activity_feed = true;
        app.switch_tab(Tab::Feed);
        assert!(!app.activity_feed);
    }

    #[test]
    fn switch_tab_clears_dms_activity() {
        let mut app = App::new("me".to_string());
        app.activity_dms = true;
        app.switch_tab(Tab::Dms);
        assert!(!app.activity_dms);
    }

    #[test]
    fn switch_tab_clears_room_activity() {
        let mut app = App::new("me".to_string());
        app.activity_rooms.insert("lobby".to_string(), true);
        app.switch_tab(Tab::Room("lobby".to_string()));
        assert!(!app.activity_rooms["lobby"]);
    }

    #[test]
    fn handle_event_new_feed_post_sets_activity_when_not_on_feed() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Terminal;
        let notify = app.handle_event(Event::NewMessage {
            message_id: "1".to_string(),
            message_type: MessageType::FeedPost,
            from: "x".to_string(),
            preview: String::new(),
        });
        assert!(notify);
        assert!(app.activity_feed);
    }

    #[test]
    fn handle_event_new_feed_post_does_not_set_activity_when_on_feed() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        let notify = app.handle_event(Event::NewMessage {
            message_id: "1".to_string(),
            message_type: MessageType::FeedPost,
            from: "x".to_string(),
            preview: String::new(),
        });
        assert!(!notify);
        assert!(!app.activity_feed);
    }

    #[test]
    fn handle_event_new_room_message_sets_activity_when_not_in_room() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        let notify = app.handle_event(Event::NewRoomMessage {
            message_id: "1".to_string(),
            room: "general".to_string(),
            from: "x".to_string(),
            preview: String::new(),
        });
        assert!(notify);
        assert_eq!(app.activity_rooms.get("general").copied(), Some(true));
    }

    #[test]
    fn handle_event_new_dm_from_other_contact_sets_activity_and_status() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Dms;
        app.following = vec!["alice".to_string(), "bob".to_string()];
        app.selected_contact = 0;
        let notify = app.handle_event(Event::NewMessage {
            message_id: "1".to_string(),
            message_type: MessageType::DmText,
            from: "bob".to_string(),
            preview: String::new(),
        });
        assert!(notify);
        assert!(app.activity_dms);
        assert!(app.status_msg.contains("bob"));
    }

    #[test]
    fn handle_event_only_notifies_once_per_unread_feed_indicator() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Terminal;
        assert!(app.handle_event(Event::NewMessage {
            message_id: "1".to_string(),
            message_type: MessageType::FeedPost,
            from: "x".to_string(),
            preview: String::new(),
        }));
        assert!(!app.handle_event(Event::NewMessage {
            message_id: "2".to_string(),
            message_type: MessageType::FeedPost,
            from: "x".to_string(),
            preview: String::new(),
        }));
    }

    #[test]
    fn clamp_selected_contact_resets_when_following_shrinks() {
        let mut app = App::new("me".to_string());
        app.following = vec!["alice".to_string(), "bob".to_string()];
        app.selected_contact = 1;
        app.following = vec!["alice".to_string()];
        app.clamp_selected_contact();
        assert_eq!(app.selected_contact, 0);
    }

    // ── Scroll ────────────────────────────────────────────────────────────────

    #[test]
    fn scroll_up_increases_offset() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        assert_eq!(app.current_scroll(), 0);
        app.scroll_up();
        assert_eq!(app.current_scroll(), SCROLL_STEP);
    }

    #[test]
    fn scroll_down_decreases_offset() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        app.scroll_up();
        app.scroll_up();
        let before = app.current_scroll();
        app.scroll_down();
        assert_eq!(app.current_scroll(), before - SCROLL_STEP);
    }

    #[test]
    fn scroll_down_clamps_at_zero() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        app.scroll_down(); // should not underflow
        assert_eq!(app.current_scroll(), 0);
    }

    #[test]
    fn scroll_is_per_tab() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        app.scroll_up();
        let feed_scroll = app.current_scroll();

        app.tab = Tab::Dms;
        assert_eq!(app.current_scroll(), 0, "Dms scroll should be independent");
        app.scroll_up();
        app.scroll_up();
        let dms_scroll = app.current_scroll();

        app.tab = Tab::Feed;
        assert_eq!(app.current_scroll(), feed_scroll, "Feed scroll unchanged");
        assert_ne!(feed_scroll, dms_scroll);
    }

    // ── all_tabs / tab_index ──────────────────────────────────────────────────

    #[test]
    fn all_tabs_includes_rooms_in_order() {
        let mut app = App::new("me".to_string());
        app.rooms = vec!["shire".to_string(), "lounge".to_string()];
        let tabs = app.all_tabs();
        assert_eq!(tabs[0], Tab::Terminal);
        assert_eq!(tabs[1], Tab::Feed);
        assert_eq!(tabs[2], Tab::Dms);
        assert_eq!(tabs[3], Tab::Room("shire".to_string()));
        assert_eq!(tabs[4], Tab::Room("lounge".to_string()));
    }

    #[test]
    fn tab_index_returns_correct_index() {
        let mut app = App::new("me".to_string());
        app.rooms = vec!["shire".to_string()];
        app.tab = Tab::Room("shire".to_string());
        assert_eq!(app.tab_index(), 3);
    }

    #[test]
    fn reset_clears_login_in_progress_and_started_at() {
        let mut app = App::new("me".to_string());
        app.auto_agent.login_in_progress = true;
        app.auto_agent.login_started_at = Some(std::time::Instant::now());
        app.auto_agent.awaiting_api_key = true;
        app.auto_agent.reset();
        assert!(!app.auto_agent.login_in_progress);
        assert!(app.auto_agent.login_started_at.is_none());
        assert!(!app.auto_agent.awaiting_api_key);
    }

    // ── terminal_tab_label ───────────────────────────────────────────────────

    #[test]
    fn terminal_tab_label_default_shell_with_pane_path_shows_directory() {
        // When the window name is a default shell (e.g. "zsh") and a pane_path
        // is available, the label should be the last directory component.
        assert_eq!(
            terminal_tab_label("zsh", Some("/Users/dev/agentbook")),
            "agentbook"
        );
        assert_eq!(
            terminal_tab_label("bash", Some("/home/user/projects/myapp")),
            "myapp"
        );
    }

    #[test]
    fn terminal_tab_label_non_default_shell_returns_shell_name() {
        // When the window name is NOT a default shell, the label should be the
        // window name itself, regardless of pane_path.
        assert_eq!(
            terminal_tab_label("vim", Some("/Users/dev/agentbook")),
            "vim"
        );
        assert_eq!(terminal_tab_label("htop", Some("/tmp")), "htop");
        assert_eq!(
            terminal_tab_label("my-custom-shell", Some("/home/user")),
            "my-custom-shell"
        );
    }

    #[test]
    fn terminal_tab_label_no_pane_path_returns_shell_name() {
        // When there's no pane_path, the label should be the window name even
        // if it's a default shell name.
        assert_eq!(terminal_tab_label("zsh", None), "zsh");
        assert_eq!(terminal_tab_label("bash", None), "bash");
        assert_eq!(terminal_tab_label("fish", None), "fish");
    }

    #[test]
    fn terminal_tab_label_case_insensitive_shell_detection() {
        // Shell detection should be case-insensitive.
        assert_eq!(
            terminal_tab_label("ZSH", Some("/Users/dev/project")),
            "project"
        );
        assert_eq!(terminal_tab_label("Bash", Some("/tmp/work")), "work");
    }
}
