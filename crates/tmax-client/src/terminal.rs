use std::io::{self, Write};

use crossterm::{
    cursor,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

/// RAII guard that restores terminal state on drop.
/// Enters alternate screen and raw mode on creation,
/// restores on drop (even on panic).
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    /// Set up the terminal for the client UI:
    /// - Enter alternate screen
    /// - Enable raw mode
    /// - Hide cursor (we'll manage cursor position ourselves)
    pub fn setup() -> anyhow::Result<Self> {
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture, cursor::Hide)?;
        terminal::enable_raw_mode()?;
        Ok(Self { _private: () })
    }

    /// Get the current terminal size as (cols, rows).
    pub fn size() -> anyhow::Result<(u16, u16)> {
        let (cols, rows) = terminal::size()?;
        Ok((cols, rows))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            cursor::Show,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = stdout.flush();
    }
}
