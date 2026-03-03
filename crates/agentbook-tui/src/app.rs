use agentbook::protocol::{Event, InboxEntry, MessageType};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

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
    pub chat_queue: Vec<String>,
    /// Cached result of `has_arda_login()` to avoid filesystem I/O in render path.
    pub cached_has_arda: bool,
    /// Environment variables to pass to child inference processes.
    pub inference_env: Vec<(String, String)>,
    /// Last time we polled for Arda login status (for auto-poll in awaiting_api_key state).
    pub last_arda_check: Option<Instant>,
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

#[derive(Debug, Clone)]
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

    /// Per-tab scroll offsets (0 = pinned to bottom/latest).
    /// Key: "feed", "dms:{node_id}", "room:{name}"
    pub scroll: HashMap<String, usize>,

    /// Username registered on the relay (fetched at startup).
    pub username: Option<String>,

    /// Message IDs we've already sent InboxAck for (avoid duplicate acks).
    pub acked_ids: HashSet<String>,
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
                chat_queue: Vec::new(),
                cached_has_arda: false,
                inference_env: Vec::new(),
                last_arda_check: None,
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
            scroll: HashMap::new(),
            username: None,
            acked_ids: HashSet::new(),
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

    /// Handle an event pushed from the node daemon.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::NewMessage { message_type, .. } => match message_type {
                MessageType::FeedPost => {
                    if self.tab != Tab::Feed {
                        self.activity_feed = true;
                    }
                }
                MessageType::DmText => {
                    if self.tab != Tab::Dms {
                        self.activity_dms = true;
                    }
                }
                MessageType::Unspecified | MessageType::RoomMessage | MessageType::RoomJoin => {}
            },
            Event::NewRoomMessage { room, .. } => {
                if self.tab != Tab::Room(room.clone()) {
                    self.activity_rooms.insert(room, true);
                }
            }
            Event::NewFollower { .. } => {}
        }
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
                let contact = self.following.get(self.selected_contact);
                self.messages
                    .iter()
                    .filter(|m| {
                        m.message_type == MessageType::DmText
                            && contact.is_none_or(|c| m.from_node_id == *c)
                    })
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
                    .map(|w| format!("{} {}", w.index + 1, w.name))
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
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let payload = serde_json::to_vec_pretty(&prefs).context("failed to encode preferences")?;
        fs::write(path, payload).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
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
        app.messages = vec![
            make_entry("alice", "hi", MessageType::DmText),
            make_entry("bob", "hey", MessageType::DmText),
        ];
        let visible = app.visible_messages();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].from_node_id, "alice");
    }

    #[test]
    fn visible_messages_dms_shows_all_when_no_following() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Dms;
        app.messages = vec![
            make_entry("x", "dm1", MessageType::DmText),
            make_entry("y", "dm2", MessageType::DmText),
        ];
        // No following → contact is None → show all DMs
        let visible = app.visible_messages();
        assert_eq!(visible.len(), 2);
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
        app.handle_event(Event::NewMessage {
            message_id: "1".to_string(),
            message_type: MessageType::FeedPost,
            from: "x".to_string(),
            preview: String::new(),
        });
        assert!(app.activity_feed);
    }

    #[test]
    fn handle_event_new_feed_post_does_not_set_activity_when_on_feed() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        app.handle_event(Event::NewMessage {
            message_id: "1".to_string(),
            message_type: MessageType::FeedPost,
            from: "x".to_string(),
            preview: String::new(),
        });
        assert!(!app.activity_feed);
    }

    #[test]
    fn handle_event_new_room_message_sets_activity_when_not_in_room() {
        let mut app = App::new("me".to_string());
        app.tab = Tab::Feed;
        app.handle_event(Event::NewRoomMessage {
            message_id: "1".to_string(),
            room: "general".to_string(),
            from: "x".to_string(),
            preview: String::new(),
        });
        assert_eq!(app.activity_rooms.get("general").copied(), Some(true));
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
}
