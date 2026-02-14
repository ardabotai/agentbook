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
