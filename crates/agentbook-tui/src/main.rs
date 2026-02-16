mod agent;
mod agent_config;
mod app;
mod ui;

use agent::{AgentMessage, AgentProcess, LoginMessage, LoginProcess};
use agent_config::{AgentConfig, AuthType, PROVIDERS};
use agentbook::client::{NodeClient, default_socket_path};
use anyhow::{Context, Result};
use app::{AgentSetupStep, App, ApprovalRequest, ChatLine, ChatRole};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "agentbook", about = "agentbook terminal chat client")]
struct Args {
    /// Path to the node daemon's Unix socket.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Disable the AI agent sidecar.
    #[arg(long)]
    no_agent: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let socket_path = args.socket.unwrap_or_else(default_socket_path);

    // Pre-flight: check if setup has been run
    if let Ok(state_dir) = agentbook_mesh::state_dir::default_state_dir() {
        let recovery_key_path = state_dir.join("recovery.key");
        if !agentbook_mesh::recovery::has_recovery_key(&recovery_key_path) {
            eprintln!("Not set up yet. Run: agentbook-cli setup");
            std::process::exit(1);
        }
    }

    // Pre-flight: check if node daemon is reachable
    let mut client = NodeClient::connect(&socket_path).await.with_context(|| {
        format!(
            "Node daemon not running. Run: agentbook-cli up\n  (failed to connect to {})",
            socket_path.display()
        )
    })?;

    let node_id = client.node_id().to_string();
    let mut app = App::new(node_id);
    app.refresh(&mut client).await;

    // Check for existing agent config or start setup wizard
    let mut agent = if !args.no_agent {
        match agent_config::load_agent_config() {
            Some(config) => {
                app.agent_config = Some(config.clone());
                match AgentProcess::spawn_with_config(
                    socket_path.to_str().unwrap_or_default(),
                    &config,
                )
                .await
                {
                    Ok(a) => {
                        app.agent_connected = true;
                        app.add_system_msg("Agent connected.".to_string());
                        Some(a)
                    }
                    Err(e) => {
                        app.add_system_msg(format!("Agent failed to start: {e}"));
                        None
                    }
                }
            }
            None => {
                // No config — start the setup wizard
                app.agent_setup = Some(AgentSetupStep::SelectProvider { selected: 0 });
                None
            }
        }
    } else {
        None
    };

    let mut login: Option<LoginProcess> = None;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(
        &mut terminal,
        &mut app,
        &mut client,
        &mut agent,
        &mut login,
        &socket_path,
    )
    .await;

    // Cleanup
    if let Some(a) = &mut agent {
        a.kill().await;
    }
    if let Some(l) = &mut login {
        l.kill().await;
    }

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    client: &mut NodeClient,
    agent: &mut Option<AgentProcess>,
    login: &mut Option<LoginProcess>,
    socket_path: &std::path::Path,
) -> Result<()> {
    let mut refresh_interval = tokio::time::interval(Duration::from_secs(5));

    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        // Poll for keyboard events with a short timeout
        tokio::select! {
            // Check keyboard events
            poll_result = tokio::task::spawn_blocking(|| event::poll(Duration::from_millis(50))) => {
                if let Ok(Ok(true)) = poll_result
                    && let Ok(Event::Key(key)) = event::read()
                {
                    handle_key(app, client, agent, login, socket_path, key).await;
                }
            }

            // Check agent messages
            agent_msg = async {
                if let Some(a) = agent {
                    a.message_rx.recv().await
                } else {
                    std::future::pending().await
                }
            } => {
                if let Some(msg) = agent_msg {
                    handle_agent_message(app, msg);
                } else {
                    app.agent_connected = false;
                    app.add_system_msg("Agent disconnected.".to_string());
                    *agent = None;
                }
            }

            // Check login process messages
            login_msg = async {
                if let Some(l) = login {
                    l.message_rx.recv().await
                } else {
                    std::future::pending().await
                }
            } => {
                if let Some(msg) = login_msg {
                    handle_login_message(app, agent, login, socket_path, msg).await;
                } else {
                    // Login process exited without result
                    if let Some(l) = login {
                        l.kill().await;
                    }
                    *login = None;
                    if app.agent_setup.is_some() {
                        app.agent_setup = Some(AgentSetupStep::SelectProvider { selected: 0 });
                        app.add_system_msg("OAuth login failed. Try again.".to_string());
                    }
                }
            }

            // Periodic refresh
            _ = refresh_interval.tick() => {
                app.refresh(client).await;
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

async fn handle_key(
    app: &mut App,
    client: &mut NodeClient,
    agent: &mut Option<AgentProcess>,
    login: &mut Option<LoginProcess>,
    socket_path: &std::path::Path,
    key: crossterm::event::KeyEvent,
) {
    // Handle setup wizard input first
    if app.agent_setup.is_some() {
        handle_setup_key(app, agent, login, socket_path, key).await;
        return;
    }

    // Handle approval Y/N first
    if app.pending_approval.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.pending_approval = None;
                app.add_system_msg("Approved.".to_string());
                if let Some(a) = agent {
                    let _ = a.send_approval(true).await;
                }
                return;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                app.pending_approval = None;
                app.add_system_msg("Denied.".to_string());
                if let Some(a) = agent {
                    let _ = a.send_approval(false).await;
                }
                return;
            }
            _ => return,
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.should_quit = true;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Tab => {
            app.toggle_view();
        }
        KeyCode::Char('1') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.view = app::View::Feed;
        }
        KeyCode::Char('2') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.view = app::View::Dms;
        }
        KeyCode::Up => {
            if app.selected_contact > 0 {
                app.selected_contact -= 1;
            }
        }
        KeyCode::Down => {
            if app.selected_contact + 1 < app.following.len() {
                app.selected_contact += 1;
            }
        }
        KeyCode::Enter => {
            if !app.input.is_empty() {
                let input = app.input.clone();
                app.input.clear();

                // Send to agent
                if let Some(a) = agent {
                    app.chat_history.push(ChatLine {
                        role: ChatRole::User,
                        text: input.clone(),
                    });
                    app.agent_typing = true;
                    let _ = a.send_message(&input).await;
                } else {
                    // No agent — send directly to node as before
                    send_message(app, client, &input).await;
                }
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(c) => {
            app.input.push(c);
        }
        _ => {}
    }
}

async fn handle_setup_key(
    app: &mut App,
    agent: &mut Option<AgentProcess>,
    login: &mut Option<LoginProcess>,
    socket_path: &std::path::Path,
    key: crossterm::event::KeyEvent,
) {
    let step = match app.agent_setup.take() {
        Some(s) => s,
        None => return,
    };

    match step {
        AgentSetupStep::SelectProvider { selected } => match key.code {
            KeyCode::Up => {
                let new = if selected > 0 {
                    selected - 1
                } else {
                    PROVIDERS.len() - 1
                };
                app.agent_setup = Some(AgentSetupStep::SelectProvider { selected: new });
            }
            KeyCode::Down => {
                let new = if selected + 1 < PROVIDERS.len() {
                    selected + 1
                } else {
                    0
                };
                app.agent_setup = Some(AgentSetupStep::SelectProvider { selected: new });
            }
            KeyCode::Enter => {
                let provider = &PROVIDERS[selected];
                match provider.auth_type {
                    AuthType::ApiKey => {
                        app.agent_setup = Some(AgentSetupStep::EnterApiKey {
                            provider_idx: selected,
                            input: String::new(),
                            masked: true,
                        });
                    }
                    AuthType::OAuth => {
                        // Spawn the login process
                        match LoginProcess::spawn(provider.provider_id).await {
                            Ok(l) => {
                                *login = Some(l);
                                app.agent_setup = Some(AgentSetupStep::OAuthWaiting {
                                    provider_idx: selected,
                                    auth_url: None,
                                    instructions: None,
                                });
                            }
                            Err(e) => {
                                app.add_system_msg(format!("Failed to start login: {e}"));
                                app.agent_setup = Some(AgentSetupStep::SelectProvider { selected });
                            }
                        }
                    }
                    AuthType::None => {
                        // No auth needed (e.g., Ollama) — save config and connect
                        let config = AgentConfig {
                            provider: provider.provider_id.to_string(),
                            model: provider.default_model.to_string(),
                            auth_type: AuthType::None,
                            api_key: None,
                            oauth_credentials: None,
                        };
                        finish_setup(app, agent, socket_path, config).await;
                    }
                }
            }
            KeyCode::Esc => {
                // Skip setup — run without agent
                app.agent_setup = None;
                app.add_system_msg("Agent setup skipped.".to_string());
            }
            _ => {
                app.agent_setup = Some(AgentSetupStep::SelectProvider { selected });
            }
        },

        AgentSetupStep::EnterApiKey {
            provider_idx,
            mut input,
            masked,
        } => match key.code {
            KeyCode::Enter => {
                if !input.is_empty() {
                    let provider = &PROVIDERS[provider_idx];
                    let config = AgentConfig {
                        provider: provider.provider_id.to_string(),
                        model: provider.default_model.to_string(),
                        auth_type: AuthType::ApiKey,
                        api_key: Some(input),
                        oauth_credentials: None,
                    };
                    finish_setup(app, agent, socket_path, config).await;
                } else {
                    app.agent_setup = Some(AgentSetupStep::EnterApiKey {
                        provider_idx,
                        input,
                        masked,
                    });
                }
            }
            KeyCode::Esc => {
                app.agent_setup = Some(AgentSetupStep::SelectProvider {
                    selected: provider_idx,
                });
            }
            KeyCode::Backspace => {
                input.pop();
                app.agent_setup = Some(AgentSetupStep::EnterApiKey {
                    provider_idx,
                    input,
                    masked,
                });
            }
            KeyCode::Char(c) => {
                input.push(c);
                app.agent_setup = Some(AgentSetupStep::EnterApiKey {
                    provider_idx,
                    input,
                    masked,
                });
            }
            _ => {
                app.agent_setup = Some(AgentSetupStep::EnterApiKey {
                    provider_idx,
                    input,
                    masked,
                });
            }
        },

        AgentSetupStep::OAuthWaiting {
            provider_idx,
            auth_url,
            instructions,
        } => match key.code {
            KeyCode::Esc => {
                if let Some(l) = login {
                    l.kill().await;
                }
                *login = None;
                app.agent_setup = Some(AgentSetupStep::SelectProvider {
                    selected: provider_idx,
                });
            }
            _ => {
                app.agent_setup = Some(AgentSetupStep::OAuthWaiting {
                    provider_idx,
                    auth_url,
                    instructions,
                });
            }
        },

        AgentSetupStep::OAuthPasteCode {
            provider_idx,
            mut input,
        } => match key.code {
            KeyCode::Enter => {
                if !input.is_empty() {
                    if let Some(l) = login {
                        let _ = l.send_code(&input).await;
                    }
                    app.agent_setup = Some(AgentSetupStep::Connecting);
                } else {
                    app.agent_setup = Some(AgentSetupStep::OAuthPasteCode {
                        provider_idx,
                        input,
                    });
                }
            }
            KeyCode::Esc => {
                if let Some(l) = login {
                    l.kill().await;
                }
                *login = None;
                app.agent_setup = Some(AgentSetupStep::SelectProvider {
                    selected: provider_idx,
                });
            }
            KeyCode::Backspace => {
                input.pop();
                app.agent_setup = Some(AgentSetupStep::OAuthPasteCode {
                    provider_idx,
                    input,
                });
            }
            KeyCode::Char(c) => {
                input.push(c);
                app.agent_setup = Some(AgentSetupStep::OAuthPasteCode {
                    provider_idx,
                    input,
                });
            }
            _ => {
                app.agent_setup = Some(AgentSetupStep::OAuthPasteCode {
                    provider_idx,
                    input,
                });
            }
        },

        AgentSetupStep::Connecting => {
            // Only allow Esc during connecting
            if key.code == KeyCode::Esc {
                if let Some(l) = login {
                    l.kill().await;
                }
                *login = None;
                app.agent_setup = Some(AgentSetupStep::SelectProvider { selected: 0 });
            } else {
                app.agent_setup = Some(AgentSetupStep::Connecting);
            }
        }
    }
}

async fn handle_login_message(
    app: &mut App,
    agent: &mut Option<AgentProcess>,
    login: &mut Option<LoginProcess>,
    socket_path: &std::path::Path,
    msg: LoginMessage,
) {
    match msg {
        LoginMessage::AuthUrl { url, instructions } => {
            if let Some(AgentSetupStep::OAuthWaiting { provider_idx, .. }) = &app.agent_setup {
                let idx = *provider_idx;
                app.agent_setup = Some(AgentSetupStep::OAuthWaiting {
                    provider_idx: idx,
                    auth_url: Some(url),
                    instructions,
                });
            }
        }
        LoginMessage::Prompt { .. } => {
            if let Some(AgentSetupStep::OAuthWaiting { provider_idx, .. }) = &app.agent_setup {
                let idx = *provider_idx;
                app.agent_setup = Some(AgentSetupStep::OAuthPasteCode {
                    provider_idx: idx,
                    input: String::new(),
                });
            }
        }
        LoginMessage::AuthResult { credentials } => {
            // Extract provider_idx before we overwrite agent_setup
            let provider_idx = match &app.agent_setup {
                Some(AgentSetupStep::Connecting) => {
                    // Try to find it from the login process context
                    None
                }
                Some(AgentSetupStep::OAuthWaiting { provider_idx, .. })
                | Some(AgentSetupStep::OAuthPasteCode { provider_idx, .. }) => Some(*provider_idx),
                _ => None,
            };

            // Kill login process
            if let Some(l) = login {
                l.kill().await;
            }
            *login = None;

            // Determine provider from whatever step we were in
            let idx = provider_idx.unwrap_or(0);
            let provider = &PROVIDERS[idx];
            let config = AgentConfig {
                provider: provider.provider_id.to_string(),
                model: provider.default_model.to_string(),
                auth_type: AuthType::OAuth,
                api_key: None,
                oauth_credentials: Some(credentials),
            };
            finish_setup(app, agent, socket_path, config).await;
        }
        LoginMessage::AuthError { error } => {
            if let Some(l) = login {
                l.kill().await;
            }
            *login = None;
            app.add_system_msg(format!("OAuth login failed: {error}"));
            app.agent_setup = Some(AgentSetupStep::SelectProvider { selected: 0 });
        }
    }
}

/// Save config and spawn the agent.
async fn finish_setup(
    app: &mut App,
    agent: &mut Option<AgentProcess>,
    socket_path: &std::path::Path,
    config: AgentConfig,
) {
    if let Err(e) = agent_config::save_agent_config(&config) {
        app.add_system_msg(format!("Failed to save config: {e}"));
    }

    app.agent_config = Some(config.clone());
    app.agent_setup = Some(AgentSetupStep::Connecting);

    match AgentProcess::spawn_with_config(socket_path.to_str().unwrap_or_default(), &config).await {
        Ok(a) => {
            *agent = Some(a);
            app.agent_connected = true;
            app.agent_setup = None;
            app.add_system_msg("Agent connected.".to_string());
        }
        Err(e) => {
            app.agent_setup = None;
            app.add_system_msg(format!("Agent failed to start: {e}"));
        }
    }
}

fn handle_agent_message(app: &mut App, msg: AgentMessage) {
    match msg {
        AgentMessage::TextDelta { delta } => {
            app.agent_typing = true;
            app.agent_buffer.push_str(&delta);
        }
        AgentMessage::ToolCall { name } => {
            app.add_system_msg(format!("Calling tool: {name}"));
        }
        AgentMessage::ToolResult { name, result } => {
            app.add_system_msg(format!("{name}: {result}"));
        }
        AgentMessage::ApprovalRequest { action, details } => {
            app.flush_agent_buffer();
            app.pending_approval = Some(ApprovalRequest { action, details });
        }
        AgentMessage::NodeEvent { event } => {
            if let Some(kind) = event.get("kind").and_then(|v| v.as_str()) {
                app.add_system_msg(format!("[event] {kind}"));
            }
        }
        AgentMessage::CredentialsUpdated { credentials } => {
            // Update stored config with refreshed credentials
            if let Some(ref mut config) = app.agent_config {
                config.oauth_credentials = Some(credentials);
                if let Err(e) = agent_config::save_agent_config(config) {
                    app.add_system_msg(format!("Failed to save refreshed credentials: {e}"));
                }
            }
        }
        AgentMessage::Done => {
            app.flush_agent_buffer();
        }
    }
}

async fn send_message(app: &mut App, client: &mut NodeClient, input: &str) {
    use agentbook::protocol::Request;

    let req = match app.view {
        app::View::Feed => Request::PostFeed {
            body: input.to_string(),
        },
        app::View::Dms => {
            let to = app
                .following
                .get(app.selected_contact)
                .cloned()
                .unwrap_or_default();
            if to.is_empty() {
                app.status_msg = "No contact selected".to_string();
                return;
            }
            Request::SendDm {
                to,
                body: input.to_string(),
            }
        }
    };

    match client.request(req).await {
        Ok(_) => {
            app.status_msg = "Sent!".to_string();
            app.refresh(client).await;
        }
        Err(e) => {
            app.status_msg = format!("Error: {e}");
        }
    }
}
