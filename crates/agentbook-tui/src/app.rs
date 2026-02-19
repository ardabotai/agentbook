use agentbook::protocol::{Event, InboxEntry, MessageType};
use std::collections::{HashMap, HashSet};

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

/// Which tab is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tab {
    Feed,
    Dms,
    Terminal,
    Room(String),
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

    /// Prefix-mode keybinding state (Ctrl+Space leader).
    pub prefix_mode: bool,
    pub prefix_mode_at: Option<std::time::Instant>,

    /// Per-tab unread activity indicators.
    pub activity_feed: bool,
    pub activity_dms: bool,
    pub activity_terminal: bool,

    /// Embedded terminal emulator (lazy-spawned).
    pub terminal: Option<crate::terminal::TerminalEmulator>,

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
            prefix_mode: false,
            prefix_mode_at: None,
            activity_feed: false,
            activity_dms: false,
            activity_terminal: false,
            terminal: None,
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
    pub fn all_tabs(&self) -> Vec<Tab> {
        let mut tabs = vec![Tab::Terminal, Tab::Feed, Tab::Dms];
        for room in &self.rooms {
            tabs.push(Tab::Room(room.clone()));
        }
        tabs
    }

    /// Index of the current tab in the all_tabs list.
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
    }

    /// Scroll down (toward newer messages). Clamps at 0.
    pub fn scroll_down(&mut self) {
        let key = self.scroll_key();
        let entry = self.scroll.entry(key).or_insert(0);
        *entry = entry.saturating_sub(SCROLL_STEP);
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
