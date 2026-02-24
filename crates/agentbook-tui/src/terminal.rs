use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use std::io::Write;
use std::sync::mpsc;

const DEFAULT_TMUX_SOCKET: &str = "agentbook";
const DEFAULT_TMUX_SESSION: &str = "main";

#[derive(Clone, Debug)]
enum BackendKind {
    LocalShell,
    Tmux { socket: String, session: String },
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

    /// Scroll up into scrollback history (older output).
    pub fn scroll_up(&mut self, rows: usize) {
        let current = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(current + rows);
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

    // Use Ctrl+A inside tmux so app-level Ctrl+B leader stays available.
    run_tmux(socket, &["set-option", "-t", session, "prefix", "C-a"])?;
    run_tmux(socket, &["unbind-key", "-T", "prefix", "C-b"])?;
    run_tmux(socket, &["bind-key", "-T", "prefix", "C-a", "send-prefix"])?;

    Ok(())
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

        assert_eq!(
            term.mux_split_vertical().expect("split should succeed"),
            true
        );
        let after_split = pane_count(&socket, &session);
        assert!(after_split >= before + 1);

        assert_eq!(
            term.mux_next_pane().expect("select pane should succeed"),
            true
        );
        assert_eq!(
            term.mux_close_pane().expect("close pane should succeed"),
            true
        );
        let after_close = pane_count(&socket, &session);
        assert!(after_close >= 1);
        assert!(after_close <= after_split);

        drop(term);
        kill_server(&socket);
    }
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
