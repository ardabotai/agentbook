use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use std::io::Write;
use std::sync::mpsc;

/// Embedded terminal emulator backed by a PTY + vt100 parser.
pub struct TerminalEmulator {
    parser: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    pty_writer: Box<dyn Write + Send>,
    pty_reader_rx: mpsc::Receiver<Vec<u8>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    size: (u16, u16),
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

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(&shell);
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
            parser: vt100::Parser::new(rows, cols, 0),
            master,
            pty_writer: writer,
            pty_reader_rx: rx,
            child,
            size: (cols, rows),
        })
    }

    /// Write raw input bytes to the PTY.
    pub fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.pty_writer.write_all(bytes)?;
        self.pty_writer.flush()?;
        Ok(())
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
