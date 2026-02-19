use crate::app::{App, Tab};
use agentbook::client::NodeWriter;
use agentbook::protocol::Request;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Prefix-mode timeout (1 second).
const PREFIX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Handle a key event. Returns true if the event was consumed.
pub async fn handle_key(app: &mut App, writer: &mut NodeWriter, key: KeyEvent) {
    // Auto-expire prefix mode.
    if app.prefix_mode
        && let Some(at) = app.prefix_mode_at
        && at.elapsed() >= PREFIX_TIMEOUT
    {
        app.prefix_mode = false;
        app.prefix_mode_at = None;
    }

    // Ctrl+Space enters prefix mode from any tab.
    if key.code == KeyCode::Char(' ') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.prefix_mode = true;
        app.prefix_mode_at = Some(std::time::Instant::now());
        return;
    }

    // Handle prefix-mode chord.
    if app.prefix_mode {
        app.prefix_mode = false;
        app.prefix_mode_at = None;
        match key.code {
            KeyCode::Char('1') => app.switch_tab(Tab::Feed),
            KeyCode::Char('2') => app.switch_tab(Tab::Dms),
            KeyCode::Char('3') => {
                app.switch_tab(Tab::Terminal);
                ensure_terminal(app);
            }
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
        return;
    }

    // On Terminal tab, forward everything to PTY.
    if app.tab == Tab::Terminal {
        if let Some(ref mut term) = app.terminal {
            if let Some(bytes) = key_to_bytes(&key) {
                let _ = term.write_input(&bytes);
            }
        } else {
            // No terminal yet — Enter spawns it.
            if key.code == KeyCode::Enter {
                ensure_terminal(app);
            }
        }
        return;
    }

    // Feed/DMs/Room tab key handling.
    match key.code {
        KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Tab => {
            let next = match &app.tab {
                Tab::Feed => Tab::Dms,
                Tab::Dms => Tab::Feed,
                Tab::Terminal => Tab::Feed,
                Tab::Room(_) => Tab::Feed,
            };
            app.switch_tab(next);
        }
        KeyCode::Up => {
            if app.tab == Tab::Dms && app.selected_contact > 0 {
                app.selected_contact -= 1;
            }
        }
        KeyCode::Down => {
            if app.tab == Tab::Dms && app.selected_contact + 1 < app.following.len() {
                app.selected_contact += 1;
            }
        }
        KeyCode::Enter => {
            if !app.input.is_empty() {
                let input = std::mem::take(&mut app.input);
                // Handle slash commands before normal send
                if input.starts_with('/') {
                    handle_slash_command(app, writer, &input).await;
                } else {
                    send_message(app, writer, &input).await;
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

/// Handle /join and /leave slash commands.
async fn handle_slash_command(app: &mut App, writer: &mut NodeWriter, input: &str) {
    let parts: Vec<&str> = input.split_whitespace().collect();
    match parts.first().copied() {
        Some("/join") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /join <room> [--passphrase <pass>]".to_string();
                return;
            }
            let room = parts[1].to_string();
            let passphrase = if parts.len() >= 4 && parts[2] == "--passphrase" {
                Some(parts[3..].join(" "))
            } else {
                None
            };
            let req = Request::JoinRoom { room, passphrase };
            match writer.send(req).await {
                Ok(()) => app.status_msg = "Joining room...".to_string(),
                Err(e) => app.status_msg = format!("Error: {e}"),
            }
        }
        Some("/leave") => {
            if parts.len() < 2 {
                app.status_msg = "Usage: /leave <room>".to_string();
                return;
            }
            let room = parts[1].to_string();
            let req = Request::LeaveRoom {
                room: room.clone(),
            };
            match writer.send(req).await {
                Ok(()) => app.status_msg = format!("Left #{room}"),
                Err(e) => app.status_msg = format!("Error: {e}"),
            }
        }
        _ => {
            app.status_msg = format!("Unknown command: {}", parts[0]);
        }
    }
}

/// Send a message directly to the node daemon.
async fn send_message(app: &mut App, writer: &mut NodeWriter, input: &str) {
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
                return;
            }
            Request::SendDm {
                to,
                body: input.to_string(),
            }
        }
        Tab::Terminal => return,
        Tab::Room(room) => {
            if input.len() > 140 {
                app.status_msg = "Room messages are limited to 140 characters".to_string();
                return;
            }
            Request::SendRoom {
                room: room.clone(),
                body: input.to_string(),
            }
        }
    };

    match writer.send(req).await {
        Ok(()) => {
            app.status_msg = "Sending...".to_string();
        }
        Err(e) => {
            app.status_msg = format!("Error: {e}");
        }
    }
}

/// Ensure the terminal emulator is spawned.
fn ensure_terminal(app: &mut App) {
    if app.terminal.is_some() {
        return;
    }
    // Default size — will be resized on next draw.
    match crate::terminal::TerminalEmulator::spawn(80, 24) {
        Ok(term) => app.terminal = Some(term),
        Err(e) => app.status_msg = format!("Failed to spawn shell: {e}"),
    }
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
