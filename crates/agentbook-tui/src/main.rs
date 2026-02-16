mod agent;
mod app;
mod ui;

use agent::{AgentMessage, AgentProcess};
use agentbook::client::{NodeClient, default_socket_path};
use anyhow::{Context, Result};
use app::{App, ApprovalRequest, ChatLine, ChatRole};
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
#[command(name = "agentbook-tui", about = "agentbook terminal chat client")]
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

    let mut client = NodeClient::connect(&socket_path).await.with_context(|| {
        format!(
            "failed to connect to node at {}. Is the daemon running?",
            socket_path.display()
        )
    })?;

    let node_id = client.node_id().to_string();
    let mut app = App::new(node_id);
    app.refresh(&mut client).await;

    // Spawn agent process
    let mut agent = if !args.no_agent {
        match AgentProcess::spawn(socket_path.to_str().unwrap_or_default()).await {
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
    } else {
        None
    };

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut app, &mut client, &mut agent).await;

    // Cleanup agent
    if let Some(a) = &mut agent {
        a.kill().await;
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
                    handle_key(app, client, agent, key).await;
                }
            }

            // Check agent messages
            agent_msg = async {
                if let Some(a) = agent {
                    a.message_rx.recv().await
                } else {
                    // Never resolves if no agent
                    std::future::pending().await
                }
            } => {
                if let Some(msg) = agent_msg {
                    handle_agent_message(app, msg);
                } else {
                    // Agent disconnected
                    app.agent_connected = false;
                    app.add_system_msg("Agent disconnected.".to_string());
                    *agent = None;
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
    key: crossterm::event::KeyEvent,
) {
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
