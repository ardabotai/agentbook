use crate::app::{App, PendingRequest, Tab, TerminalSplit};
use agentbook::client::NodeWriter;
use agentbook::protocol::{Request, WalletType};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Prefix-mode timeout (1 second).
const PREFIX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_TERMINAL_PANES: usize = 4;

/// Send a request to the daemon, setting status on error. Returns the pending
/// request kind on success, or `None` if the send failed.
async fn send_req(
    app: &mut App,
    writer: &mut NodeWriter,
    req: Request,
    kind: PendingRequest,
) -> Option<PendingRequest> {
    match writer.send(req).await {
        Ok(()) => Some(kind),
        Err(e) => {
            app.status_msg = format!("Error: {e}");
            None
        }
    }
}

/// Handle a key event. Returns `Some(PendingRequest)` if a request was sent
/// that expects a response (caller should push it to the pending queue).
pub async fn handle_key(
    app: &mut App,
    writer: &mut NodeWriter,
    key: KeyEvent,
) -> Option<PendingRequest> {
    // Auto-expire prefix mode.
    if app.prefix_mode
        && let Some(at) = app.prefix_mode_at
        && at.elapsed() >= PREFIX_TIMEOUT
    {
        app.prefix_mode = false;
        app.prefix_mode_at = None;
    }

    // Ctrl+B (or Ctrl+Space fallback) enters prefix mode from any tab.
    if is_prefix_key(&key) {
        app.prefix_mode = true;
        app.prefix_mode_at = Some(std::time::Instant::now());
        return None;
    }

    // Handle prefix-mode chord.
    if app.prefix_mode {
        app.prefix_mode = false;
        app.prefix_mode_at = None;
        match key.code {
            KeyCode::Char('1') => {
                app.switch_tab(Tab::Terminal);
                ensure_terminal(app);
            }
            KeyCode::Char('2') => app.switch_tab(Tab::Feed),
            KeyCode::Char('3') => app.switch_tab(Tab::Dms),
            // tmux-style terminal pane controls (while on Terminal tab):
            // % split vertical, " split horizontal, o cycle pane, x close pane.
            KeyCode::Char('%') => split_terminal(app, TerminalSplit::Vertical),
            KeyCode::Char('"') => split_terminal(app, TerminalSplit::Horizontal),
            KeyCode::Char('o') => focus_next_terminal(app),
            KeyCode::Char('x') => close_active_terminal(app),
            // Dynamic room tabs: 4, 5, 6, ... map to rooms by index
            KeyCode::Char(c) if c.is_ascii_digit() && c >= '4' => {
                let room_idx = (c as usize) - ('4' as usize);
                if let Some(room) = app.rooms.get(room_idx).cloned() {
                    app.switch_tab(Tab::Room(room));
                }
            }
            // Arrow keys: navigate prev/next tab
            KeyCode::Left => {
                let tabs = app.all_tabs();
                let idx = app.tab_index();
                if idx > 0 {
                    let tab = tabs[idx - 1].clone();
                    app.switch_tab(tab.clone());
                    if tab == Tab::Terminal {
                        ensure_terminal(app);
                    }
                }
            }
            KeyCode::Right => {
                let tabs = app.all_tabs();
                let idx = app.tab_index();
                if idx + 1 < tabs.len() {
                    let tab = tabs[idx + 1].clone();
                    app.switch_tab(tab.clone());
                    if tab == Tab::Terminal {
                        ensure_terminal(app);
                    }
                }
            }
            KeyCode::Esc => app.should_quit = true,
            _ => {} // unknown chord — ignore
        }
        return None;
    }

    // On Terminal tab, forward everything to PTY.
    if app.tab == Tab::Terminal {
        if let Some(term) = app.active_terminal_mut() {
            if let Some(bytes) = key_to_bytes(&key) {
                let _ = term.write_input(&bytes);
            }
        } else {
            // No terminal yet — Enter spawns it.
            if key.code == KeyCode::Enter {
                ensure_terminal(app);
            }
        }
        return None;
    }

    // Feed/DMs/Room tab key handling.
    match key.code {
        KeyCode::Esc => {
            app.should_quit = true;
            None
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
            None
        }
        KeyCode::Tab => {
            let next = match &app.tab {
                Tab::Feed => Tab::Dms,
                Tab::Dms => Tab::Feed,
                Tab::Terminal => Tab::Feed,
                Tab::Room(_) => Tab::Feed,
            };
            app.switch_tab(next.clone());
            if next == Tab::Terminal {
                ensure_terminal(app);
            }
            None
        }
        KeyCode::Up => {
            if app.tab == Tab::Dms && app.selected_contact > 0 {
                app.selected_contact -= 1;
            }
            None
        }
        KeyCode::Down => {
            if app.tab == Tab::Dms && app.selected_contact + 1 < app.following.len() {
                app.selected_contact += 1;
            }
            None
        }
        KeyCode::Enter => {
            if !app.input.is_empty() {
                let input = std::mem::take(&mut app.input);
                if input.starts_with('/') {
                    handle_slash_command(app, writer, &input).await
                } else {
                    send_message(app, writer, &input).await
                }
            } else {
                None
            }
        }
        KeyCode::Backspace => {
            app.input.pop();
            None
        }
        KeyCode::Char(c) => {
            app.input.push(c);
            None
        }
        _ => None,
    }
}

/// Handle slash commands. Returns `Some(PendingRequest)` if a request was sent.
async fn handle_slash_command(
    app: &mut App,
    writer: &mut NodeWriter,
    input: &str,
) -> Option<PendingRequest> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    match parts.first().copied() {
        // ── Existing ──────────────────────────────────────────────────────
        Some("/join") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /join <room> [--passphrase <pass>]".to_string();
                return None;
            }
            let room = parts[1].to_string();
            let passphrase = if parts.len() >= 4 && parts[2] == "--passphrase" {
                Some(parts[3..].join(" "))
            } else {
                None
            };
            app.status_msg = "Joining room...".to_string();
            send_req(
                app,
                writer,
                Request::JoinRoom { room, passphrase },
                PendingRequest::Send,
            )
            .await
        }
        Some("/leave") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /leave <room>".to_string();
                return None;
            }
            let room = parts[1].to_string();
            app.status_msg = format!("Leaving #{room}...");
            send_req(
                app,
                writer,
                Request::LeaveRoom { room },
                PendingRequest::Send,
            )
            .await
        }

        // ── Social ────────────────────────────────────────────────────────
        Some("/follow") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /follow <node_id or @username>".to_string();
                return None;
            }
            let target = parts[1].to_string();
            app.status_msg = format!("Following {target}...");
            send_req(
                app,
                writer,
                Request::Follow { target },
                PendingRequest::Send,
            )
            .await
        }
        Some("/unfollow") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /unfollow <node_id or @username>".to_string();
                return None;
            }
            let target = parts[1].to_string();
            app.status_msg = format!("Unfollowing {target}...");
            send_req(
                app,
                writer,
                Request::Unfollow { target },
                PendingRequest::Send,
            )
            .await
        }
        Some("/block") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /block <node_id or @username>".to_string();
                return None;
            }
            let target = parts[1].to_string();
            app.status_msg = format!("Blocking {target}...");
            send_req(app, writer, Request::Block { target }, PendingRequest::Send).await
        }
        Some("/lookup") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /lookup <@username>".to_string();
                return None;
            }
            let username = parts[1].trim_start_matches('@').to_string();
            app.status_msg = format!("Looking up @{username}...");
            send_req(
                app,
                writer,
                Request::LookupUsername { username },
                PendingRequest::SlashLookup,
            )
            .await
        }
        Some("/followers") => {
            app.status_msg = "Fetching followers...".to_string();
            send_req(
                app,
                writer,
                Request::Followers,
                PendingRequest::SlashFollowers,
            )
            .await
        }
        Some("/following") => {
            app.status_msg = "Fetching following...".to_string();
            send_req(
                app,
                writer,
                Request::Following,
                PendingRequest::SlashFollowing,
            )
            .await
        }

        // ── Wallet ────────────────────────────────────────────────────────
        Some("/balance") => {
            app.status_msg = "Fetching balance...".to_string();
            send_req(
                app,
                writer,
                Request::WalletBalance {
                    wallet: WalletType::Human,
                },
                PendingRequest::SlashBalance,
            )
            .await
        }
        Some("/send-eth") => {
            if parts.len() < 4 {
                app.status_msg = "Usage: /send-eth <to> <amount> <otp>".to_string();
                return None;
            }
            let to = parts[1].to_string();
            let amount = parts[2].to_string();
            let otp = parts[3].to_string();
            app.status_msg = "Sending ETH...".to_string();
            send_req(
                app,
                writer,
                Request::SendEth { to, amount, otp },
                PendingRequest::Send,
            )
            .await
        }
        Some("/send-usdc") => {
            if parts.len() < 4 {
                app.status_msg = "Usage: /send-usdc <to> <amount> <otp>".to_string();
                return None;
            }
            let to = parts[1].to_string();
            let amount = parts[2].to_string();
            let otp = parts[3].to_string();
            app.status_msg = "Sending USDC...".to_string();
            send_req(
                app,
                writer,
                Request::SendUsdc { to, amount, otp },
                PendingRequest::Send,
            )
            .await
        }

        // ── Utility ───────────────────────────────────────────────────────
        Some("/identity") => {
            app.status_msg = "Fetching identity...".to_string();
            send_req(
                app,
                writer,
                Request::Identity,
                PendingRequest::SlashIdentity,
            )
            .await
        }
        Some("/health") => {
            app.status_msg = "Checking health...".to_string();
            send_req(app, writer, Request::Health, PendingRequest::SlashHealth).await
        }
        Some("/help") => {
            app.status_msg = "Commands: /follow /unfollow /block /lookup /followers /following /balance /send-eth /send-usdc /identity /health /join /leave /help".to_string();
            None
        }

        _ => {
            app.status_msg = format!("Unknown command: {}", parts[0]);
            None
        }
    }
}

/// Send a message directly to the node daemon.
async fn send_message(
    app: &mut App,
    writer: &mut NodeWriter,
    input: &str,
) -> Option<PendingRequest> {
    let req = match &app.tab {
        Tab::Feed => Request::PostFeed {
            body: input.to_string(),
        },
        Tab::Dms => {
            let to = app
                .following
                .get(app.selected_contact)
                .cloned()
                .unwrap_or_default();
            if to.is_empty() {
                app.status_msg = "No contact selected".to_string();
                return None;
            }
            Request::SendDm {
                to,
                body: input.to_string(),
            }
        }
        Tab::Terminal => return None,
        Tab::Room(room) => {
            if input.len() > 140 {
                app.status_msg = "Room messages are limited to 140 characters".to_string();
                return None;
            }
            Request::SendRoom {
                room: room.clone(),
                body: input.to_string(),
            }
        }
    };

    app.status_msg = "Sending...".to_string();
    send_req(app, writer, req, PendingRequest::Send).await
}

/// Ensure the terminal emulator is spawned.
fn ensure_terminal(app: &mut App) {
    if !app.terminals.is_empty() {
        return;
    }
    // Default size — will be resized on next draw.
    match crate::terminal::TerminalEmulator::spawn(80, 24) {
        Ok(term) => {
            app.terminals.push(term);
            app.active_terminal = 0;
            app.terminal_split = TerminalSplit::Single;
        }
        Err(e) => app.status_msg = format!("Failed to spawn shell: {e}"),
    }
}

fn split_terminal(app: &mut App, direction: TerminalSplit) {
    if app.tab != Tab::Terminal {
        return;
    }
    ensure_terminal(app);
    if let Some(term) = app.active_terminal_mut()
        && term.is_persistent_mux()
    {
        let result = match direction {
            TerminalSplit::Vertical => term.mux_split_vertical(),
            TerminalSplit::Horizontal => term.mux_split_horizontal(),
            TerminalSplit::Single => Ok(false),
        };
        match result {
            Ok(true) => {
                app.status_msg = match direction {
                    TerminalSplit::Vertical => "tmux split vertical".to_string(),
                    TerminalSplit::Horizontal => "tmux split horizontal".to_string(),
                    TerminalSplit::Single => String::new(),
                };
            }
            Ok(false) => {}
            Err(e) => app.status_msg = format!("tmux split failed: {e}"),
        }
        return;
    }
    if app.terminals.len() >= MAX_TERMINAL_PANES {
        app.status_msg = format!("Pane limit reached ({MAX_TERMINAL_PANES})");
        return;
    }
    match crate::terminal::TerminalEmulator::spawn(80, 24) {
        Ok(term) => {
            app.terminals.push(term);
            app.active_terminal = app.terminals.len().saturating_sub(1);
            app.terminal_split = direction;
            app.status_msg = format!(
                "Split {} ({}/{MAX_TERMINAL_PANES})",
                match direction {
                    TerminalSplit::Vertical => "vertical",
                    TerminalSplit::Horizontal => "horizontal",
                    TerminalSplit::Single => "single",
                },
                app.terminals.len()
            );
        }
        Err(e) => app.status_msg = format!("Failed to split terminal: {e}"),
    }
}

fn focus_next_terminal(app: &mut App) {
    if app.tab != Tab::Terminal || app.terminals.len() < 2 {
        if app.tab == Tab::Terminal
            && let Some(term) = app.active_terminal_mut()
            && term.is_persistent_mux()
        {
            match term.mux_next_pane() {
                Ok(true) | Ok(false) => {}
                Err(e) => app.status_msg = format!("tmux pane switch failed: {e}"),
            }
        }
        return;
    }
    app.active_terminal = (app.active_terminal + 1) % app.terminals.len();
}

fn close_active_terminal(app: &mut App) {
    if app.tab != Tab::Terminal || app.terminals.is_empty() {
        return;
    }
    if let Some(term) = app.active_terminal_mut()
        && term.is_persistent_mux()
    {
        match term.mux_close_pane() {
            Ok(true) => app.status_msg = "tmux pane closed".to_string(),
            Ok(false) => {}
            Err(e) => app.status_msg = format!("tmux close failed: {e}"),
        }
        return;
    }
    app.terminals.remove(app.active_terminal);
    if app.terminals.is_empty() {
        app.active_terminal = 0;
        app.terminal_split = TerminalSplit::Single;
        app.status_msg = "Closed terminal pane".to_string();
        return;
    }
    if app.active_terminal >= app.terminals.len() {
        app.active_terminal = app.terminals.len().saturating_sub(1);
    }
    if app.terminals.len() == 1 {
        app.terminal_split = TerminalSplit::Single;
    }
}

fn is_prefix_key(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(' ') | KeyCode::Char('b'))
}

/// Convert a crossterm KeyEvent to raw bytes for the PTY.
fn key_to_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Helper: wrap bytes with ESC prefix for Alt modifier.
    let with_alt = |bytes: Vec<u8>| -> Vec<u8> {
        if alt {
            let mut out = vec![0x1b];
            out.extend(bytes);
            out
        } else {
            bytes
        }
    };

    match key.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl+A..Z → 0x01..0x1A
            let byte = c.to_ascii_lowercase() as u8;
            if byte.is_ascii_lowercase() {
                Some(with_alt(vec![byte - b'a' + 1]))
            } else {
                None
            }
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            Some(with_alt(s.as_bytes().to_vec()))
        }
        KeyCode::Enter => Some(with_alt(b"\r".to_vec())),
        KeyCode::Backspace => Some(with_alt(b"\x7f".to_vec())),
        KeyCode::Tab => Some(with_alt(b"\t".to_vec())),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Esc => Some(b"\x1b".to_vec()),
        KeyCode::Up if shift => Some(b"\x1b[1;2A".to_vec()),
        KeyCode::Down if shift => Some(b"\x1b[1;2B".to_vec()),
        KeyCode::Right if shift => Some(b"\x1b[1;2C".to_vec()),
        KeyCode::Left if shift => Some(b"\x1b[1;2D".to_vec()),
        KeyCode::Up if alt => Some(b"\x1b[1;3A".to_vec()),
        KeyCode::Down if alt => Some(b"\x1b[1;3B".to_vec()),
        KeyCode::Right if alt => Some(b"\x1b[1;3C".to_vec()),
        KeyCode::Left if alt => Some(b"\x1b[1;3D".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::F(n) => {
            let seq = match n {
                1 => "\x1bOP",
                2 => "\x1bOQ",
                3 => "\x1bOR",
                4 => "\x1bOS",
                5 => "\x1b[15~",
                6 => "\x1b[17~",
                7 => "\x1b[18~",
                8 => "\x1b[19~",
                9 => "\x1b[20~",
                10 => "\x1b[21~",
                11 => "\x1b[23~",
                12 => "\x1b[24~",
                _ => return None,
            };
            Some(seq.as_bytes().to_vec())
        }
        _ => None,
    }
}
