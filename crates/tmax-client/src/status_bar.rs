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
    git_branch: Option<&str>,
    input_mode: InputMode,
) -> anyhow::Result<()> {
    queue!(stdout, cursor::MoveTo(0, row))?;

    // Build status bar content
    let short_id = if session_id.len() > 8 {
        &session_id[..8]
    } else {
        session_id
    };

    let mut parts = Vec::new();
    parts.push(format!(" {short_id}"));

    if let Some(l) = label {
        parts.push(l.to_string());
    }

    parts.push(format!("[{mode_label}]"));

    if let Some(branch) = git_branch {
        parts.push(format!("@{branch}"));
    }

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

    // Pad to full width
    let padded = if content.len() < cols as usize {
        format!("{content:<width$}", width = cols as usize)
    } else {
        content[..cols as usize].to_string()
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
        git_branch: Option<&str>,
        input_mode: InputMode,
        cols: u16,
    ) -> String {
        let mut buf = Vec::new();
        render_status_bar(&mut buf, 0, cols, session_id, label, mode_label, git_branch, input_mode)
            .unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }

    #[test]
    fn status_bar_shows_short_session_id() {
        let output = render_to_string(
            "abcdef12-3456-7890-abcd-ef1234567890",
            None, "EDIT", None, InputMode::Normal, 80,
        );
        assert!(output.contains("abcdef12"), "should show first 8 chars of session ID");
        assert!(!output.contains("abcdef12-3456"), "should NOT show full session ID");
    }

    #[test]
    fn status_bar_shows_label() {
        let output = render_to_string(
            "abcdef12", Some("my-shell"), "EDIT", None, InputMode::Normal, 80,
        );
        assert!(output.contains("my-shell"), "should show label");
    }

    #[test]
    fn status_bar_shows_edit_mode() {
        let output = render_to_string(
            "abcdef12", None, "EDIT", None, InputMode::Normal, 80,
        );
        assert!(output.contains("[EDIT]"), "should show [EDIT]");
    }

    #[test]
    fn status_bar_shows_view_mode() {
        let output = render_to_string(
            "abcdef12", None, "VIEW", None, InputMode::Normal, 80,
        );
        assert!(output.contains("[VIEW]"), "should show [VIEW]");
    }

    #[test]
    fn status_bar_shows_git_branch() {
        let output = render_to_string(
            "abcdef12", None, "EDIT", Some("feat/my-branch"), InputMode::Normal, 80,
        );
        assert!(output.contains("@feat/my-branch"), "should show git branch");
    }

    #[test]
    fn status_bar_shows_prefix_indicator() {
        let output = render_to_string(
            "abcdef12", None, "EDIT", None, InputMode::Prefix, 80,
        );
        assert!(output.contains("[PREFIX]"), "should show [PREFIX] in prefix mode");
    }

    #[test]
    fn status_bar_hides_prefix_in_normal_mode() {
        let output = render_to_string(
            "abcdef12", None, "EDIT", None, InputMode::Normal, 80,
        );
        assert!(!output.contains("[PREFIX]"), "should NOT show [PREFIX] in normal mode");
    }

    #[test]
    fn status_bar_short_id_passthrough() {
        let output = render_to_string(
            "short", None, "EDIT", None, InputMode::Normal, 80,
        );
        assert!(output.contains("short"), "short IDs should pass through unchanged");
    }

    #[test]
    fn status_bar_all_fields() {
        let output = render_to_string(
            "abcdef12-long-id", Some("worker-1"), "VIEW",
            Some("main"), InputMode::Prefix, 120,
        );
        assert!(output.contains("abcdef12"));
        assert!(output.contains("worker-1"));
        assert!(output.contains("[VIEW]"));
        assert!(output.contains("@main"));
        assert!(output.contains("[PREFIX]"));
    }
}
