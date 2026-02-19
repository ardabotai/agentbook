mod app;
mod input;
mod terminal;
mod ui;

use agentbook::client::{NodeClient, default_socket_path};
use agentbook::protocol::{InboxEntry, Request, Response, RoomInfo};
use anyhow::{Context, Result};
use app::{App, Tab};
use clap::Parser;
use crossterm::event::{self, Event};
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
    let client = NodeClient::connect(&socket_path).await.with_context(|| {
        format!(
            "Node daemon not running. Run: agentbook-cli up\n  (failed to connect to {})",
            socket_path.display()
        )
    })?;

    let node_id = client.node_id().to_string();
    let (mut writer, mut reader) = client.into_split();

    let mut app = App::new(node_id);

    // Spawn the terminal immediately since it's the first tab.
    match terminal::TerminalEmulator::spawn(80, 24) {
        Ok(term) => app.terminal = Some(term),
        Err(e) => eprintln!("Warning: failed to spawn shell: {e}"),
    }

    // Initial data load — send requests, responses handled in event loop.
    let _ = writer
        .send(Request::Inbox {
            unread_only: false,
            limit: Some(100),
        })
        .await;
    let _ = writer.send(Request::Following).await;
    let _ = writer.send(Request::ListRooms).await;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut app, &mut writer, &mut reader).await;

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Tracks what kind of response we're expecting from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingRequest {
    Inbox,
    Following,
    Send,
    ListRooms,
    RoomInbox(String),
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    writer: &mut agentbook::client::NodeWriter,
    reader: &mut agentbook::client::NodeReader,
) -> Result<()> {
    let mut refresh_interval = tokio::time::interval(Duration::from_secs(30));
    let mut pending: Vec<PendingRequest> = vec![
        PendingRequest::Inbox,
        PendingRequest::Following,
        PendingRequest::ListRooms,
    ];

    // FPS cap for terminal rendering — 16ms ≈ 60fps.
    let mut last_draw = std::time::Instant::now();
    let min_draw_interval = Duration::from_millis(16);

    loop {
        // Resize PTY to match actual inner area before drawing.
        if app.tab == Tab::Terminal
            && let Some(ref mut term) = app.terminal
        {
            let size = terminal.size()?;
            // Inner area: full height minus tab bar (1) + status bar (1) + borders (2).
            let cols = size.width.saturating_sub(2);
            let rows = size.height.saturating_sub(4);
            term.resize(cols, rows);
        }

        // Draw at most 60fps.
        if last_draw.elapsed() >= min_draw_interval {
            terminal.draw(|f| ui::draw(f, app))?;
            last_draw = std::time::Instant::now();
        }

        tokio::select! {
            // Keyboard events
            poll_result = tokio::task::spawn_blocking(|| event::poll(Duration::from_millis(16))) => {
                if let Ok(Ok(true)) = poll_result
                    && let Ok(evt) = event::read()
                {
                    match evt {
                        Event::Key(key) => {
                            if input::handle_key(app, writer, key).await {
                                pending.push(PendingRequest::Send);
                            }
                        }
                        Event::Resize(_, _) => {
                            // Terminal widget will pick up new size on next draw.
                            // Resize PTY if active.
                            if let Some(ref mut term) = app.terminal {
                                let size = terminal.size()?;
                                // Inner area: full height minus tab bar (1) + status bar (1) + borders (2).
                                let cols = size.width.saturating_sub(2);
                                let rows = size.height.saturating_sub(4);
                                term.resize(cols, rows);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Socket responses and events from the node daemon.
            response = reader.next() => {
                match response {
                    Some(Ok(Response::Event { event })) => {
                        // Refresh room inbox if it's a room message event.
                        if let agentbook::protocol::Event::NewRoomMessage { ref room, .. } = event {
                            let room = room.clone();
                            let _ = writer.send(Request::RoomInbox {
                                room: room.clone(),
                                limit: Some(200),
                            }).await;
                            pending.push(PendingRequest::RoomInbox(room));
                        }
                        app.handle_event(event);
                        // Auto-refresh inbox on new message events.
                        let _ = writer.send(Request::Inbox {
                            unread_only: false,
                            limit: Some(100),
                        }).await;
                        pending.push(PendingRequest::Inbox);
                    }
                    Some(Ok(Response::Ok { data })) => {
                        if !pending.is_empty() {
                            let kind = pending.remove(0);
                            handle_ok_response(app, writer, &mut pending, kind, data).await;
                        }
                    }
                    Some(Ok(Response::Error { message, .. })) => {
                        if !pending.is_empty() {
                            let kind = pending.remove(0);
                            if kind == PendingRequest::Send {
                                app.status_msg = format!("Error: {message}");
                            }
                        }
                    }
                    Some(Ok(Response::Hello { .. })) => {
                        // Ignore duplicate hellos.
                    }
                    Some(Err(e)) => {
                        app.status_msg = format!("Socket error: {e}");
                    }
                    None => {
                        app.status_msg = "Daemon disconnected".to_string();
                        app.should_quit = true;
                    }
                }
            }

            // Periodic refresh (longer interval since events push now).
            _ = refresh_interval.tick() => {
                let _ = writer.send(Request::Inbox {
                    unread_only: false,
                    limit: Some(100),
                }).await;
                let _ = writer.send(Request::Following).await;
                pending.push(PendingRequest::Inbox);
                pending.push(PendingRequest::Following);
            }
        }

        // Process PTY output (non-blocking).
        if let Some(ref mut term) = app.terminal
            && term.process_output()
            && app.tab != Tab::Terminal
        {
            app.activity_terminal = true;
        }

        // Check if shell exited.
        if let Some(ref mut term) = app.terminal
            && !term.is_alive()
        {
            if let Some(code) = term.exit_status() {
                app.status_msg = format!("Shell exited (status {code}). Ctrl+Space 1 to restart.");
            }
            app.terminal = None;
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

async fn handle_ok_response(
    app: &mut App,
    writer: &mut agentbook::client::NodeWriter,
    pending: &mut Vec<PendingRequest>,
    kind: PendingRequest,
    data: Option<serde_json::Value>,
) {
    match kind {
        PendingRequest::Inbox => {
            if let Some(data) = data
                && let Ok(entries) = serde_json::from_value::<Vec<InboxEntry>>(data)
            {
                app.messages = entries;
            }
        }
        PendingRequest::Following => {
            if let Some(data) = data
                && let Ok(list) = serde_json::from_value::<Vec<serde_json::Value>>(data)
            {
                app.following = list
                    .iter()
                    .filter_map(|v| v.get("node_id").and_then(|n| n.as_str()).map(String::from))
                    .collect();
            }
        }
        PendingRequest::Send => {
            app.status_msg = "Sent!".to_string();
        }
        PendingRequest::ListRooms => {
            if let Some(data) = data
                && let Ok(rooms) = serde_json::from_value::<Vec<RoomInfo>>(data)
            {
                app.rooms = rooms.iter().map(|r| r.room.clone()).collect();
                app.secure_rooms = rooms
                    .iter()
                    .filter(|r| r.secure)
                    .map(|r| r.room.clone())
                    .collect();
                // Fetch inbox for each room.
                for room in &app.rooms {
                    let _ = writer
                        .send(Request::RoomInbox {
                            room: room.clone(),
                            limit: Some(200),
                        })
                        .await;
                    pending.push(PendingRequest::RoomInbox(room.clone()));
                }
            }
        }
        PendingRequest::RoomInbox(room) => {
            if let Some(data) = data
                && let Ok(entries) = serde_json::from_value::<Vec<InboxEntry>>(data)
            {
                app.room_messages.insert(room, entries);
            }
        }
    }
}
