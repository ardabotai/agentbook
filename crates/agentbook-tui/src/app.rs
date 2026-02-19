use agentbook::protocol::{Event, InboxEntry, MessageType};
use std::collections::{HashMap, HashSet};

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
