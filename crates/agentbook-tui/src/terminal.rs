use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use std::io::Write;
use std::sync::mpsc;

const DEFAULT_TMUX_SOCKET: &str = "agentbook";
const DEFAULT_TMUX_SESSION: &str = "main";

/// Mouse button identifier for PTY event forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// A mouse event to forward to the PTY child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEvent {
    Press(MouseButton),
    Release(MouseButton),
    Drag(MouseButton),
    Motion,
}

/// Map a mouse button to its xterm button code (0=left, 1=middle, 2=right).
fn mouse_button_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

#[derive(Clone, Debug)]
enum BackendKind {
    LocalShell,
    Tmux { socket: String, session: String },
}

#[derive(Clone, Debug)]
pub struct MuxWindow {
    pub index: usize,
    pub name: String,
    pub active: bool,
    /// Current working directory of the active pane in this window.
    pub pane_path: Option<String>,
}

/// Cloneable snapshot source for background automation work.
#[derive(Clone, Debug)]
pub enum AutomationSnapshotSource {
    Local {
        text: String,
    },
    Tmux {
        socket: String,
        session: String,
        max_lines: usize,
    },
}

#[derive(Clone, Debug)]
pub struct AutomationWindowSnapshot {
    pub index: usize,
    pub name: String,
    pub active: bool,
    pub text: String,
}

impl AutomationSnapshotSource {
    pub fn collect(&self) -> Result<Vec<AutomationWindowSnapshot>> {
        match self {
            Self::Local { text } => Ok(vec![AutomationWindowSnapshot {
                index: 0,
                name: "shell".to_string(),
                active: true,
                text: text.clone(),
            }]),
            Self::Tmux {
                socket,
                session,
                max_lines,
            } => {
                let out = run_tmux_capture(
                    socket,
                    &[
                        "list-windows",
                        "-t",
                        session,
                        "-F",
                        "#{window_index}\t#{window_name}\t#{window_active}",
                    ],
                )?;
                let mut snapshots = Vec::new();
                for line in out.lines() {
                    let mut parts = line.splitn(3, '\t');
                    let Some(index) = parts.next() else { continue };
                    let Some(name) = parts.next() else { continue };
                    let Some(active) = parts.next() else { continue };
                    let Ok(index) = index.parse::<usize>() else {
                        continue;
                    };
                    let text = run_tmux_capture(
                        socket,
                        &[
                            "capture-pane",
                            "-p",
                            "-t",
                            &format!("{session}:{index}"),
                            "-S",
                            &format!("-{}", max_lines),
                        ],
                    )?;
                    snapshots.push(AutomationWindowSnapshot {
                        index,
                        name: name.to_string(),
                        active: active == "1",
                        text,
                    });
                }
                Ok(snapshots)
            }
        }
    }
}

/// Embedded terminal emulator backed by a PTY + vt100 parser.
pub struct TerminalEmulator {
    parser: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    pty_writer: Box<dyn Write + Send>,
    pty_reader_rx: mpsc::Receiver<Vec<u8>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    size: (u16, u16),
    backend: BackendKind,
}

impl TerminalEmulator {
    /// Spawn a new PTY running `$SHELL` (fallback `/bin/bash`).
    pub fn spawn(cols: u16, rows: u16) -> Result<Self> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open PTY")?;

        let backend = if tmux_enabled() {
            let socket = tmux_socket_name();
            let session = tmux_session_name();
            setup_tmux_session(&socket, &session)?;
            BackendKind::Tmux { socket, session }
        } else {
            BackendKind::LocalShell
        };

        let mut cmd = match &backend {
            BackendKind::LocalShell => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
                CommandBuilder::new(&shell)
            }
            BackendKind::Tmux { socket, session } => {
                let mut cmd = CommandBuilder::new("tmux");
                cmd.arg("-L");
                cmd.arg(socket);
                cmd.arg("attach-session");
                cmd.arg("-t");
                cmd.arg(session);
                cmd
            }
        };
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell")?;
        drop(pair.slave); // close slave side in parent

        let writer = pair.master.take_writer().context("PTY writer")?;
        let mut reader = pair.master.try_clone_reader().context("PTY reader")?;
        let master = pair.master;

        let (tx, rx) = mpsc::channel();

        // Background thread reads PTY output and sends chunks.
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            parser: vt100::Parser::new(rows, cols, 10_000),
            master,
            pty_writer: writer,
            pty_reader_rx: rx,
            child,
            size: (cols, rows),
            backend,
        })
    }

    /// Write raw input bytes to the PTY. Also snaps back to live view.
    pub fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.scroll_to_bottom();
        self.pty_writer.write_all(bytes)?;
        self.pty_writer.flush()?;
        Ok(())
    }

    /// Send a mouse wheel event to the PTY using the currently active xterm
    /// mouse protocol mode/encoding. Returns `Ok(true)` when forwarded, or
    /// `Ok(false)` if mouse reporting is not enabled by the child app.
    pub fn write_mouse_wheel(
        &mut self,
        column_1based: u16,
        row_1based: u16,
        up: bool,
    ) -> Result<bool> {
        let mode = self.parser.screen().mouse_protocol_mode();
        if mode == vt100::MouseProtocolMode::None {
            return Ok(false);
        }

        let encoding = self.parser.screen().mouse_protocol_encoding();
        let button_code = if up { 64 } else { 65 }; // xterm wheel up/down
        let col = column_1based.max(1);
        let row = row_1based.max(1);

        let bytes = match encoding {
            vt100::MouseProtocolEncoding::Sgr => {
                format!("\u{1b}[<{button_code};{col};{row}M").into_bytes()
            }
            vt100::MouseProtocolEncoding::Utf8 => {
                if col > 2015 || row > 2015 {
                    // UTF-8 protocol can encode larger coordinates than default, but cap to a
                    // practical range and let local scrollback fallback when exceeded.
                    return Ok(false);
                }
                let cx = char::from_u32(u32::from(col) + 32).context("invalid utf8 mouse col")?;
                let cy = char::from_u32(u32::from(row) + 32).context("invalid utf8 mouse row")?;
                let mut out = vec![0x1b, b'[', b'M', button_code + 32];
                let mut buf = [0u8; 4];
                out.extend(cx.encode_utf8(&mut buf).as_bytes());
                out.extend(cy.encode_utf8(&mut buf).as_bytes());
                out
            }
            vt100::MouseProtocolEncoding::Default => {
                // Legacy X10 encoding only supports coordinates up to 223 (32+223 <= 255).
                if col > 223 || row > 223 {
                    return Ok(false);
                }
                vec![
                    0x1b,
                    b'[',
                    b'M',
                    button_code + 32,
                    (col as u8) + 32,
                    (row as u8) + 32,
                ]
            }
        };

        self.write_input(&bytes)?;
        Ok(true)
    }

    /// Send a mouse button/motion event to the PTY using the currently active
    /// xterm mouse protocol mode/encoding. Returns `Ok(true)` when forwarded,
    /// or `Ok(false)` if the child hasn't enabled the appropriate mouse mode.
    pub fn write_mouse_event(
        &mut self,
        column_1based: u16,
        row_1based: u16,
        event: MouseEvent,
    ) -> Result<bool> {
        let mode = self.parser.screen().mouse_protocol_mode();

        // Check whether the current mode accepts this kind of event.
        let accepted = match &event {
            MouseEvent::Press(_) => mode != vt100::MouseProtocolMode::None,
            MouseEvent::Release(_) => matches!(
                mode,
                vt100::MouseProtocolMode::PressRelease
                    | vt100::MouseProtocolMode::ButtonMotion
                    | vt100::MouseProtocolMode::AnyMotion
            ),
            MouseEvent::Drag(_) => matches!(
                mode,
                vt100::MouseProtocolMode::ButtonMotion | vt100::MouseProtocolMode::AnyMotion
            ),
            MouseEvent::Motion => mode == vt100::MouseProtocolMode::AnyMotion,
        };
        if !accepted {
            return Ok(false);
        }

        let encoding = self.parser.screen().mouse_protocol_encoding();
        let col = column_1based.max(1);
        let row = row_1based.max(1);

        // xterm button codes: 0=left, 1=middle, 2=right
        let (button_base, is_release) = match &event {
            MouseEvent::Press(b) | MouseEvent::Drag(b) => (mouse_button_code(*b), false),
            MouseEvent::Release(b) => (mouse_button_code(*b), true),
            MouseEvent::Motion => (0, false), // no button held
        };

        // Motion/drag adds 32 to the button code.
        let motion_flag: u8 = match &event {
            MouseEvent::Drag(_) | MouseEvent::Motion => 32,
            _ => 0,
        };

        let button_code = button_base + motion_flag;

        let bytes = match encoding {
            vt100::MouseProtocolEncoding::Sgr => {
                let suffix = if is_release { 'm' } else { 'M' };
                format!("\u{1b}[<{button_code};{col};{row}{suffix}").into_bytes()
            }
            vt100::MouseProtocolEncoding::Utf8 => {
                if col > 2015 || row > 2015 {
                    return Ok(false);
                }
                let cx = char::from_u32(u32::from(col) + 32).context("invalid utf8 mouse col")?;
                let cy = char::from_u32(u32::from(row) + 32).context("invalid utf8 mouse row")?;
                // In non-SGR encodings, release is encoded as button code 3.
                let code = if is_release { 3 } else { button_code };
                let mut out = vec![0x1b, b'[', b'M', code + 32];
                let mut buf = [0u8; 4];
                out.extend(cx.encode_utf8(&mut buf).as_bytes());
                out.extend(cy.encode_utf8(&mut buf).as_bytes());
                out
            }
            vt100::MouseProtocolEncoding::Default => {
                if col > 223 || row > 223 {
                    return Ok(false);
                }
                let code = if is_release { 3 } else { button_code };
                vec![
                    0x1b,
                    b'[',
                    b'M',
                    code + 32,
                    (col as u8) + 32,
                    (row as u8) + 32,
                ]
            }
        };

        self.write_input(&bytes)?;
        Ok(true)
    }

    /// Scroll up into scrollback history (older output).
    pub fn scroll_up(&mut self, rows: usize) {
        let current = self.parser.screen().scrollback();
        self.parser
            .screen_mut()
            .set_scrollback(current.saturating_add(rows));
    }

    /// Scroll down toward the live view.
    pub fn scroll_down(&mut self, rows: usize) {
        let current = self.parser.screen().scrollback();
        self.parser
            .screen_mut()
            .set_scrollback(current.saturating_sub(rows));
    }

    /// Snap back to the live (bottom) view.
    pub fn scroll_to_bottom(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    /// Returns true when the user has scrolled into history (not at live view).
    pub fn is_scrolled_back(&self) -> bool {
        self.parser.screen().scrollback() > 0
    }

    /// Drain pending output from the PTY reader and feed into the parser.
    /// Returns true if any bytes were processed (i.e. screen may have changed).
    pub fn process_output(&mut self) -> bool {
        let mut any = false;
        while let Ok(chunk) = self.pty_reader_rx.try_recv() {
            self.parser.process(&chunk);
            any = true;
        }
        any
    }

    /// Resize the PTY and parser.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == self.size || cols == 0 || rows == 0 {
            return;
        }
        self.size = (cols, rows);
        self.parser.screen_mut().set_size(rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Get the vt100 screen for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Reset parser render cache to a blank screen.
    ///
    /// Useful when switching tmux panes/windows to avoid stale glyph artifacts
    /// between full redraws.
    pub fn reset_screen(&mut self) {
        self.parser = vt100::Parser::new(self.size.1, self.size.0, 10_000);
    }

    /// Return a plain-text snapshot of visible rows from the current screen.
    ///
    /// The newest lines are kept when `max_lines` is smaller than the screen
    /// height.
    pub fn snapshot_text(&self, max_lines: usize) -> String {
        let screen = self.screen();
        let rows = self.size.1 as usize;
        let cols = self.size.0 as usize;
        if rows == 0 || cols == 0 {
            return String::new();
        }

        let start_row = rows.saturating_sub(max_lines);
        let mut lines = Vec::new();
        for row in start_row..rows {
            let mut line = String::new();
            for col in 0..cols {
                let ch = screen
                    .cell(row as u16, col as u16)
                    .map(|c| c.contents())
                    .unwrap_or(" ");
                line.push_str(if ch.is_empty() { " " } else { ch });
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }

    /// Build a cloneable source for background automation scans.
    pub fn automation_snapshot_source(&self, max_lines: usize) -> AutomationSnapshotSource {
        match &self.backend {
            BackendKind::LocalShell => AutomationSnapshotSource::Local {
                text: self.snapshot_text(max_lines),
            },
            BackendKind::Tmux { socket, session } => AutomationSnapshotSource::Tmux {
                socket: socket.clone(),
                session: session.clone(),
                max_lines,
            },
        }
    }

    /// Returns true when this terminal is backed by a persistent tmux session.
    pub fn is_persistent_mux(&self) -> bool {
        matches!(self.backend, BackendKind::Tmux { .. })
    }

    /// tmux-backed split operation. Returns false on non-tmux backends.
    pub fn mux_split_vertical(&self) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(
            socket,
            &["split-window", "-h", "-t", &format!("{session}:.")],
        )?;
        Ok(true)
    }

    /// tmux-backed split operation. Returns false on non-tmux backends.
    pub fn mux_split_horizontal(&self) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(
            socket,
            &["split-window", "-v", "-t", &format!("{session}:.")],
        )?;
        Ok(true)
    }

    /// tmux-backed next-pane operation. Returns false on non-tmux backends.
    pub fn mux_next_pane(&self) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(socket, &["select-pane", "-t", &format!("{session}:.+")])?;
        Ok(true)
    }

    /// tmux-backed close-pane operation. Returns false on non-tmux backends.
    pub fn mux_close_pane(&self) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(socket, &["kill-pane", "-t", &format!("{session}:.")])?;
        Ok(true)
    }

    /// tmux-backed select-pane by character-cell coordinate in the current
    /// window. Returns false on non-tmux backends or no pane hit.
    pub fn mux_select_pane_at(&self, col: u16, row: u16) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        let out = run_tmux_capture(
            socket,
            &[
                "list-panes",
                "-t",
                &format!("{session}:."),
                "-F",
                "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{pane_right}\t#{pane_bottom}",
            ],
        )?;
        let col = usize::from(col);
        let row = usize::from(row);

        for line in out.lines() {
            let mut parts = line.splitn(5, '\t');
            let Some(pane_id) = parts.next() else {
                continue;
            };
            let Some(left) = parts.next() else { continue };
            let Some(top) = parts.next() else { continue };
            let Some(right) = parts.next() else { continue };
            let Some(bottom) = parts.next() else { continue };
            let (Ok(left), Ok(top), Ok(right), Ok(bottom)) = (
                left.parse::<usize>(),
                top.parse::<usize>(),
                right.parse::<usize>(),
                bottom.parse::<usize>(),
            ) else {
                continue;
            };
            if col >= left && col <= right && row >= top && row <= bottom {
                run_tmux(socket, &["select-pane", "-t", pane_id])?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// tmux-backed next-window operation. Returns false on non-tmux backends.
    pub fn mux_next_window(&self) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(socket, &["next-window", "-t", session])?;
        Ok(true)
    }

    /// tmux-backed previous-window operation. Returns false on non-tmux backends.
    pub fn mux_prev_window(&self) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(socket, &["previous-window", "-t", session])?;
        Ok(true)
    }

    /// tmux-backed new-window operation. Returns false on non-tmux backends.
    pub fn mux_new_window(&self) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(socket, &["new-window", "-t", session])?;
        Ok(true)
    }

    /// tmux-backed close current window operation. Returns false on non-tmux backends.
    pub fn mux_close_window(&self) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(socket, &["kill-window", "-t", &format!("{session}:.")])?;
        Ok(true)
    }

    /// tmux-backed rename-window operation. Returns false on non-tmux backends.
    pub fn mux_rename_window(&self, index: usize, name: &str) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(
            socket,
            &["rename-window", "-t", &format!("{session}:{index}"), name],
        )?;
        Ok(true)
    }

    /// tmux-backed select-window operation. Returns false on non-tmux backends.
    pub fn mux_select_window(&self, index: usize) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        run_tmux(
            socket,
            &["select-window", "-t", &format!("{session}:{index}")],
        )?;
        Ok(true)
    }

    /// Send literal keys to a specific tmux window. Returns false on non-tmux backends.
    pub fn mux_send_window_keys(&self, index: usize, keys: &str) -> Result<bool> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(false);
        };
        let target = format!("{session}:{index}");
        let mut parts = keys.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                run_tmux(socket, &["send-keys", "-t", &target, "-l", part])?;
            }
            if parts.peek().is_some() {
                run_tmux(socket, &["send-keys", "-t", &target, "Enter"])?;
            }
        }
        Ok(true)
    }

    /// List tmux windows (index/name/active). Returns `None` on non-tmux backends.
    pub fn mux_windows(&self) -> Result<Option<Vec<MuxWindow>>> {
        let BackendKind::Tmux { socket, session } = &self.backend else {
            return Ok(None);
        };
        let out = run_tmux_capture(
            socket,
            &[
                "list-windows",
                "-t",
                session,
                "-F",
                "#{window_index}\t#{window_name}\t#{window_active}\t#{pane_current_path}",
            ],
        )?;
        let mut windows = Vec::new();
        for line in out.lines() {
            let mut parts = line.splitn(4, '\t');
            let Some(idx) = parts.next() else { continue };
            let Some(name) = parts.next() else { continue };
            let Some(active) = parts.next() else { continue };
            let pane_path = parts
                .next()
                .map(|p| p.to_string())
                .filter(|p| !p.is_empty());
            let Ok(index) = idx.parse::<usize>() else {
                continue;
            };
            windows.push(MuxWindow {
                index,
                name: name.to_string(),
                active: active == "1",
                pane_path,
            });
        }
        windows.sort_by_key(|w| w.index);
        Ok(Some(windows))
    }

    /// Check if the child shell is still alive.
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// Get the exit status if the child exited.
    pub fn exit_status(&mut self) -> Option<u32> {
        self.child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.exit_code())
    }
}

impl Drop for TerminalEmulator {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn tmux_enabled() -> bool {
    let env_pref = std::env::var("AGENTBOOK_TMUX").unwrap_or_else(|_| "1".to_string());
    if env_pref == "0" || env_pref.eq_ignore_ascii_case("false") {
        return false;
    }
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tmux_socket_name() -> String {
    std::env::var("AGENTBOOK_TMUX_SOCKET").unwrap_or_else(|_| DEFAULT_TMUX_SOCKET.to_string())
}

fn tmux_session_name() -> String {
    std::env::var("AGENTBOOK_TMUX_SESSION").unwrap_or_else(|_| DEFAULT_TMUX_SESSION.to_string())
}

fn setup_tmux_session(socket: &str, session: &str) -> Result<()> {
    // Create detached session if missing.
    let has = std::process::Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .arg("has-session")
        .arg("-t")
        .arg(session)
        .output()
        .context("failed to probe tmux session")?
        .status
        .success();

    if !has {
        run_tmux(socket, &["new-session", "-d", "-s", session])?;
    }

    // Use Ctrl+A inside tmux so app-level Ctrl+Space leader stays available.
    run_tmux(socket, &["set-option", "-t", session, "prefix", "C-a"])?;
    run_tmux(socket, &["unbind-key", "-T", "prefix", "C-b"])?;
    run_tmux(socket, &["bind-key", "-T", "prefix", "C-a", "send-prefix"])?;

    Ok(())
}

fn run_tmux(socket: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(args)
        .status()
        .with_context(|| format!("failed to run tmux {}", args.join(" ")))?;
    if !status.success() {
        anyhow::bail!("tmux command failed: {}", args.join(" "));
    }
    Ok(())
}

fn run_tmux_capture(socket: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(args)
        .output()
        .with_context(|| format!("failed to run tmux {}", args.join(" ")))?;
    if !out.status.success() {
        anyhow::bail!("tmux command failed: {}", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn has_tmux() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn unique_name(prefix: &str) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        format!("{prefix}-{}-{now}", std::process::id())
    }

    fn kill_server(socket: &str) {
        let _ = std::process::Command::new("tmux")
            .arg("-L")
            .arg(socket)
            .arg("kill-server")
            .output();
    }

    fn has_session(socket: &str, session: &str) -> bool {
        std::process::Command::new("tmux")
            .arg("-L")
            .arg(socket)
            .arg("has-session")
            .arg("-t")
            .arg(session)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn pane_count(socket: &str, session: &str) -> usize {
        let out = std::process::Command::new("tmux")
            .arg("-L")
            .arg(socket)
            .arg("list-panes")
            .arg("-t")
            .arg(session)
            .arg("-F")
            .arg("#{pane_id}")
            .output()
            .expect("failed to list tmux panes");
        if !out.status.success() {
            return 0;
        }
        String::from_utf8_lossy(&out.stdout).lines().count()
    }

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: tests serialize env mutation under a global mutex.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: tests serialize env mutation under a global mutex.
            unsafe {
                if let Some(v) = &self.old {
                    std::env::set_var(self.key, v);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn spawn_uses_local_shell_when_tmux_disabled() {
        let _guard = env_lock().lock().expect("env lock poisoned");
        let _tmux = EnvGuard::set("AGENTBOOK_TMUX", "0");
        let term = TerminalEmulator::spawn(80, 24).expect("spawn should succeed");
        assert!(!term.is_persistent_mux());
    }

    #[test]
    fn tmux_backend_persists_across_terminal_instances() {
        if !has_tmux() {
            return;
        }
        let _guard = env_lock().lock().expect("env lock poisoned");
        let socket = unique_name("agentbook-test-sock");
        let session = unique_name("agentbook-test-session");
        let _tmux = EnvGuard::set("AGENTBOOK_TMUX", "1");
        let _socket = EnvGuard::set("AGENTBOOK_TMUX_SOCKET", &socket);
        let _session = EnvGuard::set("AGENTBOOK_TMUX_SESSION", &session);
        kill_server(&socket);

        let term1 = TerminalEmulator::spawn(80, 24).expect("tmux-backed spawn should succeed");
        assert!(term1.is_persistent_mux());
        assert!(has_session(&socket, &session));
        drop(term1);

        // Session should outlive the first attached client.
        assert!(has_session(&socket, &session));

        let term2 = TerminalEmulator::spawn(80, 24).expect("reattach should succeed");
        assert!(term2.is_persistent_mux());
        assert!(has_session(&socket, &session));
        drop(term2);

        kill_server(&socket);
    }

    #[test]
    fn tmux_mux_commands_manage_panes() {
        if !has_tmux() {
            return;
        }
        let _guard = env_lock().lock().expect("env lock poisoned");
        let socket = unique_name("agentbook-test-sock");
        let session = unique_name("agentbook-test-session");
        let _tmux = EnvGuard::set("AGENTBOOK_TMUX", "1");
        let _socket = EnvGuard::set("AGENTBOOK_TMUX_SOCKET", &socket);
        let _session = EnvGuard::set("AGENTBOOK_TMUX_SESSION", &session);
        kill_server(&socket);

        let term = TerminalEmulator::spawn(80, 24).expect("tmux-backed spawn should succeed");
        assert!(term.is_persistent_mux());
        let before = pane_count(&socket, &session);
        assert!(before >= 1);

        assert!(term.mux_split_vertical().expect("split should succeed"));
        let after_split = pane_count(&socket, &session);
        assert!(after_split > before);

        assert!(term.mux_next_pane().expect("select pane should succeed"));
        assert!(term.mux_close_pane().expect("close pane should succeed"));
        let after_close = pane_count(&socket, &session);
        assert!(after_close >= 1);
        assert!(after_close <= after_split);

        drop(term);
        kill_server(&socket);
    }
}
