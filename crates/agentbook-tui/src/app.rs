use crate::agent_config;
use agentbook::client::NodeClient;
use agentbook::protocol::{InboxEntry, MessageType, Request};

/// Which view is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Feed,
    Dms,
}

/// A single line in the agent conversation panel.
#[derive(Debug, Clone)]
pub struct ChatLine {
    pub role: ChatRole,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Agent,
    System,
}

/// Pending approval request from the agent.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub action: String,
    pub details: String,
}

/// Steps of the agent inference setup wizard.
#[derive(Debug, Clone)]
pub enum AgentSetupStep {
    /// User is choosing a provider from the list.
    SelectProvider { selected: usize },
    /// User is entering an API key.
    EnterApiKey {
        provider_idx: usize,
        input: String,
        masked: bool,
    },
    /// Waiting for the OAuth flow — URL displayed, waiting for agent to prompt.
    OAuthWaiting {
        provider_idx: usize,
        auth_url: Option<String>,
        instructions: Option<String>,
    },
    /// User is pasting the OAuth authorization code.
    OAuthPasteCode { provider_idx: usize, input: String },
    /// Agent is connecting after setup.
    Connecting,
}

/// The TUI application state.
pub struct App {
    pub view: View,
    pub input: String,
    pub messages: Vec<InboxEntry>,
    pub following: Vec<String>,
    pub selected_contact: usize,
    pub node_id: String,
    pub status_msg: String,
    pub should_quit: bool,

    // Agent state
    pub chat_history: Vec<ChatLine>,
    pub agent_typing: bool,
    pub agent_buffer: String,
    pub pending_approval: Option<ApprovalRequest>,
    pub agent_connected: bool,

    // Agent setup wizard
    pub agent_setup: Option<AgentSetupStep>,
    pub agent_config: Option<agent_config::AgentConfig>,
}

impl App {
    pub fn new(node_id: String) -> Self {
        Self {
            view: View::Feed,
            input: String::new(),
            messages: Vec::new(),
            following: Vec::new(),
            selected_contact: 0,
            node_id,
            status_msg: String::new(),
            should_quit: false,
            chat_history: Vec::new(),
            agent_typing: false,
            agent_buffer: String::new(),
            pending_approval: None,
            agent_connected: false,
            agent_setup: None,
            agent_config: None,
        }
    }

    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Feed => View::Dms,
            View::Dms => View::Feed,
        };
    }

    /// Refresh inbox and following list from the node daemon.
    pub async fn refresh(&mut self, client: &mut NodeClient) {
        // Fetch inbox
        if let Ok(Some(data)) = client
            .request(Request::Inbox {
                unread_only: false,
                limit: Some(100),
            })
            .await
            && let Ok(entries) = serde_json::from_value::<Vec<InboxEntry>>(data)
        {
            self.messages = entries;
        }

        // Fetch following list
        if let Ok(Some(data)) = client.request(Request::Following).await
            && let Ok(list) = serde_json::from_value::<Vec<serde_json::Value>>(data)
        {
            self.following = list
                .iter()
                .filter_map(|v| v.get("node_id").and_then(|n| n.as_str()).map(String::from))
                .collect();
        }
    }

    /// Get messages filtered for the current view.
    pub fn visible_messages(&self) -> Vec<&InboxEntry> {
        match self.view {
            View::Feed => self
                .messages
                .iter()
                .filter(|m| m.message_type == MessageType::FeedPost)
                .collect(),
            View::Dms => {
                let contact = self.following.get(self.selected_contact);
                self.messages
                    .iter()
                    .filter(|m| {
                        m.message_type == MessageType::DmText
                            && contact.is_none_or(|c| m.from_node_id == *c)
                    })
                    .collect()
            }
        }
    }

    /// Flush the agent's streaming buffer into a chat line.
    pub fn flush_agent_buffer(&mut self) {
        if !self.agent_buffer.is_empty() {
            self.chat_history.push(ChatLine {
                role: ChatRole::Agent,
                text: std::mem::take(&mut self.agent_buffer),
            });
        }
        self.agent_typing = false;
    }

    /// Add a system message to the chat history.
    pub fn add_system_msg(&mut self, text: String) {
        self.chat_history.push(ChatLine {
            role: ChatRole::System,
            text,
        });
    }
}
