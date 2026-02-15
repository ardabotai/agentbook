use std::io::Write;

use crossterm::{cursor, queue, style};

use crate::keybindings::InputMode;

/// Render the status bar at the given row.
pub fn render_status_bar(
    stdout: &mut impl Write,
    row: u16,
    cols: u16,
    session_id: &str,
    label: Option<&str>,
    mode_label: &str,
    input_mode: InputMode,
) -> anyhow::Result<()> {
    queue!(stdout, cursor::MoveTo(0, row))?;

    // Build status bar content
    let short_id: String = session_id.chars().take(8).collect();

    let mut parts = Vec::new();
    parts.push(format!(" {}", short_id));

    if let Some(l) = label {
        parts.push(l.to_string());
    }

    parts.push(format!("[{mode_label}]"));

    if input_mode == InputMode::Prefix {
        parts.push("[PREFIX]".to_string());
    }

    let content = parts.join("  ");

    // Render with inverted colors for visibility
    queue!(
        stdout,
        style::SetAttribute(style::Attribute::Reset),
        style::SetAttribute(style::Attribute::Reverse),
    )?;

    // Pad to full width (use char count, not byte length, for UTF-8 safety)
    let char_count = content.chars().count();
    let col_width = cols as usize;
    let padded = if char_count < col_width {
        format!("{content:<width$}", width = col_width)
    } else {
        content.chars().take(col_width).collect::<String>()
    };

    queue!(stdout, style::Print(padded))?;
    queue!(stdout, style::SetAttribute(style::Attribute::Reset))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_to_string(
        session_id: &str,
        label: Option<&str>,
        mode_label: &str,
        input_mode: InputMode,
        cols: u16,
    ) -> String {
        let mut buf = Vec::new();
        render_status_bar(&mut buf, 0, cols, session_id, label, mode_label, input_mode)
            .unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }

    #[test]
    fn status_bar_shows_short_session_id() {
        let output = render_to_string(
            "abcdef12-3456-7890-abcd-ef1234567890",
            None, "EDIT", InputMode::Normal, 80,
        );
        assert!(output.contains("abcdef12"), "should show first 8 chars of session ID");
        assert!(!output.contains("abcdef12-3456"), "should NOT show full session ID");
    }

    #[test]
    fn status_bar_shows_label() {
        let output = render_to_string(
            "abcdef12", Some("my-shell"), "EDIT", InputMode::Normal, 80,
        );
        assert!(output.contains("my-shell"), "should show label");
    }

    #[test]
    fn status_bar_shows_edit_mode() {
        let output = render_to_string(
            "abcdef12", None, "EDIT", InputMode::Normal, 80,
        );
        assert!(output.contains("[EDIT]"), "should show [EDIT]");
    }

    #[test]
    fn status_bar_shows_view_mode() {
        let output = render_to_string(
            "abcdef12", None, "VIEW", InputMode::Normal, 80,
        );
        assert!(output.contains("[VIEW]"), "should show [VIEW]");
    }

    #[test]
    fn status_bar_shows_prefix_indicator() {
        let output = render_to_string(
            "abcdef12", None, "EDIT", InputMode::Prefix, 80,
        );
        assert!(output.contains("[PREFIX]"), "should show [PREFIX] in prefix mode");
    }

    #[test]
    fn status_bar_hides_prefix_in_normal_mode() {
        let output = render_to_string(
            "abcdef12", None, "EDIT", InputMode::Normal, 80,
        );
        assert!(!output.contains("[PREFIX]"), "should NOT show [PREFIX] in normal mode");
    }

    #[test]
    fn status_bar_short_id_passthrough() {
        let output = render_to_string(
            "short", None, "EDIT", InputMode::Normal, 80,
        );
        assert!(output.contains("short"), "short IDs should pass through unchanged");
    }

    #[test]
    fn status_bar_multibyte_utf8_no_panic() {
        // Session ID with emoji (4-byte UTF-8 chars)
        let output = render_to_string(
            "\u{1F600}\u{1F601}\u{1F602}\u{1F603}\u{1F604}\u{1F605}\u{1F606}\u{1F607}\u{1F608}\u{1F609}",
            None, "EDIT", InputMode::Normal, 80,
        );
        // Should contain first 8 emoji without panicking
        assert!(output.contains("\u{1F600}"), "should handle multi-byte session ID");

        // Very narrow terminal that forces truncation through multi-byte content
        let output2 = render_to_string(
            "\u{1F600}\u{1F601}\u{1F602}", None, "EDIT", InputMode::Normal, 5,
        );
        assert!(!output2.is_empty(), "should not panic on narrow cols with multi-byte content");

        // CJK characters in label
        let output3 = render_to_string(
            "abcdef12", Some("\u{4F60}\u{597D}\u{4E16}\u{754C}"), "EDIT", InputMode::Normal, 15,
        );
        assert!(!output3.is_empty(), "should not panic with CJK label and truncation");
    }

    #[test]
    fn status_bar_all_fields() {
        let output = render_to_string(
            "abcdef12-long-id", Some("worker-1"), "VIEW",
            InputMode::Prefix, 120,
        );
        assert!(output.contains("abcdef12"));
        assert!(output.contains("worker-1"));
        assert!(output.contains("[VIEW]"));
        assert!(output.contains("[PREFIX]"));
    }
}
