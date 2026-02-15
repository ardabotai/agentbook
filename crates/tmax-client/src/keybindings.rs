use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

/// The prefix key timeout duration.
const PREFIX_TIMEOUT: Duration = Duration::from_secs(2);

/// Input mode state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// All input forwarded to PTY. Ctrl+Space enters Prefix mode.
    Normal,
    /// Waiting for a command key after Ctrl+Space.
    Prefix,
}

/// Actions that can result from processing a key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Forward the key event as raw input to the PTY.
    ForwardInput(Vec<u8>),
    /// Detach from the session.
    Detach,
    /// No action (key consumed but nothing to do).
    None,
}

/// Manages the prefix key state machine.
pub struct KeyHandler {
    mode: InputMode,
    prefix_entered_at: Option<Instant>,
    /// Whether the client is in view mode (no input forwarding).
    view_mode: bool,
}

impl KeyHandler {
    pub fn new(view_mode: bool) -> Self {
        Self {
            mode: InputMode::Normal,
            prefix_entered_at: None,
            view_mode,
        }
    }

    /// Returns the current input mode.
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// Check if prefix mode has timed out. Call this periodically.
    /// Returns true if mode was reset due to timeout.
    pub fn check_timeout(&mut self) -> bool {
        if self.mode == InputMode::Prefix {
            if let Some(entered_at) = self.prefix_entered_at {
                if entered_at.elapsed() >= PREFIX_TIMEOUT {
                    self.mode = InputMode::Normal;
                    self.prefix_entered_at = None;
                    return true;
                }
            }
        }
        false
    }

    /// Process a key event and return the resulting action.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        match self.mode {
            InputMode::Normal => self.handle_normal(key),
            InputMode::Prefix => self.handle_prefix(key),
        }
    }

    fn handle_normal(&mut self, key: KeyEvent) -> Action {
        // Check for Ctrl+Space (prefix key)
        // Ctrl+Space sends KeyCode::Char(' ') with CONTROL modifier
        if is_ctrl_space(&key) {
            self.mode = InputMode::Prefix;
            self.prefix_entered_at = Some(Instant::now());
            return Action::None;
        }

        // In view mode, drop all non-prefix input
        if self.view_mode {
            return Action::None;
        }

        // Forward input to PTY
        Action::ForwardInput(key_to_bytes(&key))
    }

    fn handle_prefix(&mut self, key: KeyEvent) -> Action {
        self.mode = InputMode::Normal;
        self.prefix_entered_at = None;

        // Ctrl+Space, Ctrl+Space -> send literal Ctrl+Space to PTY
        if is_ctrl_space(&key) {
            if self.view_mode {
                return Action::None;
            }
            return Action::ForwardInput(vec![0x00]); // NUL = Ctrl+Space
        }

        match key.code {
            // Detach (available in both edit and view mode)
            KeyCode::Char('d') => Action::Detach,

            // Unrecognized: forward the key to PTY (edit mode only)
            _ => {
                if self.view_mode {
                    Action::None
                } else {
                    Action::ForwardInput(key_to_bytes(&key))
                }
            }
        }
    }
}

/// Check if a key event is Ctrl+Space.
fn is_ctrl_space(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char(' ') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Convert a crossterm KeyEvent to raw bytes for sending to PTY.
fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    // Handle control characters
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(c) = key.code {
            let ctrl_byte = (c as u8).wrapping_sub(b'`');
            if ctrl_byte < 32 {
                return vec![ctrl_byte];
            }
        }
    }

    // Handle Alt modifier: prepend ESC (0x1b) before the character bytes.
    // This enables Alt+key combinations used in bash/zsh (e.g., Alt+b for
    // word-back, Alt+f for word-forward).
    if key.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char(c) = key.code {
            let mut bytes = vec![0x1b];
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            bytes.extend_from_slice(s.as_bytes());
            return bytes;
        }
    }

    match key.code {
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            s.as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => vec![],
        },
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Null => vec![0x00],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Press)
    }

    #[test]
    fn normal_mode_forwards_input() {
        let mut handler = KeyHandler::new(false);
        let action = handler.handle_key(make_key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(action, Action::ForwardInput(b"a".to_vec()));
    }

    #[test]
    fn ctrl_space_enters_prefix_mode() {
        let mut handler = KeyHandler::new(false);
        let action = handler.handle_key(make_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert_eq!(action, Action::None);
        assert_eq!(handler.mode(), InputMode::Prefix);
    }

    #[test]
    fn prefix_d_detaches() {
        let mut handler = KeyHandler::new(false);
        handler.handle_key(make_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        let action = handler.handle_key(make_key(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(action, Action::Detach);
        assert_eq!(handler.mode(), InputMode::Normal);
    }

    #[test]
    fn prefix_unknown_key_forwards() {
        let mut handler = KeyHandler::new(false);
        handler.handle_key(make_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        let action = handler.handle_key(make_key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(action, Action::ForwardInput(b"z".to_vec()));
        assert_eq!(handler.mode(), InputMode::Normal);
    }

    #[test]
    fn double_ctrl_space_sends_nul() {
        let mut handler = KeyHandler::new(false);
        handler.handle_key(make_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        let action = handler.handle_key(make_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert_eq!(action, Action::ForwardInput(vec![0x00]));
    }

    #[test]
    fn view_mode_drops_input() {
        let mut handler = KeyHandler::new(true);
        let action = handler.handle_key(make_key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(action, Action::None);
    }

    #[test]
    fn view_mode_allows_prefix_detach() {
        let mut handler = KeyHandler::new(true);
        handler.handle_key(make_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        let action = handler.handle_key(make_key(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(action, Action::Detach);
    }

    #[test]
    fn view_mode_prefix_unknown_key_drops() {
        let mut handler = KeyHandler::new(true);
        handler.handle_key(make_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        let action = handler.handle_key(make_key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(action, Action::None);
    }

    #[test]
    fn prefix_timeout_resets() {
        let mut handler = KeyHandler::new(false);
        handler.handle_key(make_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert_eq!(handler.mode(), InputMode::Prefix);

        // Simulate timeout by setting entered_at to the past
        handler.prefix_entered_at = Some(Instant::now() - Duration::from_secs(3));
        assert!(handler.check_timeout());
        assert_eq!(handler.mode(), InputMode::Normal);
    }

    #[test]
    fn enter_key_produces_cr() {
        let bytes = key_to_bytes(&make_key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(bytes, vec![b'\r']);
    }

    #[test]
    fn arrow_keys_produce_escape_sequences() {
        let bytes = key_to_bytes(&make_key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(bytes, b"\x1b[A".to_vec());
    }

    #[test]
    fn ctrl_c_produces_etx() {
        let bytes = key_to_bytes(&make_key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(bytes, vec![0x03]);
    }

    #[test]
    fn alt_b_produces_esc_b() {
        let bytes = key_to_bytes(&make_key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(bytes, vec![0x1b, b'b']);
    }

    #[test]
    fn alt_f_produces_esc_f() {
        let bytes = key_to_bytes(&make_key(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(bytes, vec![0x1b, b'f']);
    }

    #[test]
    fn alt_d_produces_esc_d() {
        let bytes = key_to_bytes(&make_key(KeyCode::Char('d'), KeyModifiers::ALT));
        assert_eq!(bytes, vec![0x1b, b'd']);
    }

    #[test]
    fn alt_dot_produces_esc_dot() {
        let bytes = key_to_bytes(&make_key(KeyCode::Char('.'), KeyModifiers::ALT));
        assert_eq!(bytes, vec![0x1b, b'.']);
    }
}
