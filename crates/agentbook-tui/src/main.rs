mod app;
mod input;
mod terminal;
mod ui;

use agentbook::client::{NodeClient, default_socket_path};
use agentbook::protocol::{
    FollowInfo, HealthStatus, IdentityInfo, InboxEntry, Request, Response, RoomInfo,
    UsernameLookup, WalletInfo,
};
use anyhow::{Context, Result};
use app::{App, PendingRequest, Tab};
use clap::Parser;
use crossterm::event::{self, Event, MouseEventKind};
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
            eprintln!("Not set up yet. Run: agentbook setup");
            std::process::exit(1);
        }
    }

    // Pre-flight: check if node daemon is reachable
    let client = NodeClient::connect(&socket_path).await.with_context(|| {
        format!(
            "Node daemon not running. Run: agentbook up\n  (failed to connect to {})",
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
    let _ = writer.send(Request::Identity).await;
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
    crossterm::execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut app, &mut writer, &mut reader).await;

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    writer: &mut agentbook::client::NodeWriter,
    reader: &mut agentbook::client::NodeReader,
) -> Result<()> {
    let mut refresh_interval = tokio::time::interval(Duration::from_secs(30));
    let mut pending: Vec<PendingRequest> = vec![
        PendingRequest::Identity,
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

            // Auto-ack visible unread messages on Feed/DMs tabs.
            if matches!(app.tab, Tab::Feed | Tab::Dms) {
                let to_ack: Vec<String> = app
                    .visible_messages()
                    .iter()
                    .filter(|m| !m.acked && !app.acked_ids.contains(&m.message_id))
                    .map(|m| m.message_id.clone())
                    .collect();

                for msg_id in to_ack {
                    app.acked_ids.insert(msg_id.clone());
                    // Optimistically mark as read in local state.
                    if let Some(entry) = app.messages.iter_mut().find(|m| m.message_id == msg_id) {
                        entry.acked = true;
                    }
                    let _ = writer
                        .send(Request::InboxAck {
                            message_id: msg_id,
                        })
                        .await;
                    pending.push(PendingRequest::InboxAck);
                }
            }
        }

        tokio::select! {
            // Keyboard events
            poll_result = tokio::task::spawn_blocking(|| event::poll(Duration::from_millis(16))) => {
                if let Ok(Ok(true)) = poll_result
                    && let Ok(evt) = event::read()
                {
                    match evt {
                        Event::Key(key) => {
                            if let Some(kind) = input::handle_key(app, writer, key).await {
                                pending.push(kind);
                            }
                        }
                        Event::Mouse(mouse) => {
                            match mouse.kind {
                                MouseEventKind::ScrollUp => {
                                    if app.tab == Tab::Terminal {
                                        if let Some(ref mut term) = app.terminal {
                                            term.scroll_up(app::SCROLL_STEP);
                                        }
                                    } else {
                                        app.scroll_up();
                                    }
                                }
                                MouseEventKind::ScrollDown => {
                                    if app.tab == Tab::Terminal {
                                        if let Some(ref mut term) = app.terminal {
                                            term.scroll_down(app::SCROLL_STEP);
                                        }
                                    } else {
                                        app.scroll_down();
                                    }
                                }
                                _ => {}
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
                            // Show errors for all user-initiated commands (not background refreshes).
                            if !matches!(
                                kind,
                                PendingRequest::Inbox
                                    | PendingRequest::Following
                                    | PendingRequest::ListRooms
                                    | PendingRequest::RoomInbox(_)
                                    | PendingRequest::Identity
                                    | PendingRequest::InboxAck
                            ) {
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
        PendingRequest::Identity => {
            if let Some(data) = data
                && let Ok(info) = serde_json::from_value::<IdentityInfo>(data)
            {
                app.username = info.username;
            }
        }
        PendingRequest::InboxAck => {
            // Nothing to do — ack was optimistic.
        }
        PendingRequest::SlashIdentity => {
            if let Some(data) = data
                && let Ok(info) = serde_json::from_value::<IdentityInfo>(data)
            {
                let name = info.username.as_deref().unwrap_or("(none)");
                let short_id = if info.node_id.len() > 12 {
                    &info.node_id[..12]
                } else {
                    &info.node_id
                };
                app.status_msg = format!("Identity: {short_id}… @{name}");
            }
        }
        PendingRequest::SlashHealth => {
            if let Some(data) = data
                && let Ok(h) = serde_json::from_value::<HealthStatus>(data)
            {
                let relay = if h.relay_connected { "ok" } else { "down" };
                app.status_msg = format!(
                    "Health: relay {relay} | following {} | unread {}",
                    h.following_count, h.unread_count
                );
            }
        }
        PendingRequest::SlashBalance => {
            if let Some(data) = data
                && let Ok(w) = serde_json::from_value::<WalletInfo>(data)
            {
                app.status_msg = format!("Balance: {} ETH | {} USDC", w.eth_balance, w.usdc_balance);
            }
        }
        PendingRequest::SlashLookup => {
            if let Some(data) = data
                && let Ok(lu) = serde_json::from_value::<UsernameLookup>(data)
            {
                let short_id = if lu.node_id.len() > 12 {
                    &lu.node_id[..12]
                } else {
                    &lu.node_id
                };
                app.status_msg = format!("@{} -> {short_id}…", lu.username);
            }
        }
        PendingRequest::SlashFollowers => {
            if let Some(data) = data
                && let Ok(list) = serde_json::from_value::<Vec<FollowInfo>>(data)
            {
                app.status_msg = format!("Followers: {}", list.len());
            }
        }
        PendingRequest::SlashFollowing => {
            if let Some(data) = data
                && let Ok(list) = serde_json::from_value::<Vec<FollowInfo>>(data)
            {
                app.status_msg = format!("Following: {}", list.len());
            }
        }
    }
}
