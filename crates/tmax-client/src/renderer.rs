use std::io::Write;

use crossterm::{cursor, queue, style, terminal};

/// Render the full vt100 screen to the terminal.
/// Used for initial render or after resize.
pub fn render_full(
    stdout: &mut impl Write,
    screen: &vt100::Screen,
    offset_x: u16,
    offset_y: u16,
    width: u16,
    height: u16,
) -> anyhow::Result<()> {
    let (rows, cols) = screen.size();
    let render_rows = height.min(rows);
    let render_cols = width.min(cols);

    for row in 0..render_rows {
        queue!(stdout, cursor::MoveTo(offset_x, offset_y + row))?;
        let mut prev_fg = vt100::Color::Default;
        let mut prev_bg = vt100::Color::Default;
        let mut prev_bold = false;
        let mut prev_underline = false;
        let mut prev_italic = false;
        let mut prev_inverse = false;
        let mut prev_dim = false;

        // Reset attributes at start of each row
        queue!(stdout, style::SetAttribute(style::Attribute::Reset))?;

        for col in 0..render_cols {
            if let Some(cell) = screen.cell(row, col) {
                // Skip wide character continuation cells
                if cell.is_wide_continuation() {
                    continue;
                }

                // Apply style changes only when they differ
                let fg = cell.fgcolor();
                let bg = cell.bgcolor();
                let bold = cell.bold();
                let underline = cell.underline();
                let italic = cell.italic();
                let inverse = cell.inverse();
                let dim = cell.dim();

                if fg != prev_fg {
                    queue!(stdout, style::SetForegroundColor(convert_color(fg)))?;
                    prev_fg = fg;
                }
                if bg != prev_bg {
                    queue!(stdout, style::SetBackgroundColor(convert_color(bg)))?;
                    prev_bg = bg;
                }
                if bold != prev_bold {
                    queue!(
                        stdout,
                        style::SetAttribute(if bold {
                            style::Attribute::Bold
                        } else {
                            style::Attribute::NormalIntensity
                        })
                    )?;
                    prev_bold = bold;
                }
                if dim != prev_dim {
                    queue!(
                        stdout,
                        style::SetAttribute(if dim {
                            style::Attribute::Dim
                        } else {
                            style::Attribute::NormalIntensity
                        })
                    )?;
                    prev_dim = dim;
                }
                if italic != prev_italic {
                    queue!(
                        stdout,
                        style::SetAttribute(if italic {
                            style::Attribute::Italic
                        } else {
                            style::Attribute::NoItalic
                        })
                    )?;
                    prev_italic = italic;
                }
                if underline != prev_underline {
                    queue!(
                        stdout,
                        style::SetAttribute(if underline {
                            style::Attribute::Underlined
                        } else {
                            style::Attribute::NoUnderline
                        })
                    )?;
                    prev_underline = underline;
                }
                if inverse != prev_inverse {
                    queue!(
                        stdout,
                        style::SetAttribute(if inverse {
                            style::Attribute::Reverse
                        } else {
                            style::Attribute::NoReverse
                        })
                    )?;
                    prev_inverse = inverse;
                }

                let contents = cell.contents();
                if contents.is_empty() {
                    queue!(stdout, style::Print(' '))?;
                } else {
                    queue!(stdout, style::Print(contents))?;
                }
            } else {
                queue!(stdout, style::Print(' '))?;
            }
        }
    }

    // Reset attributes after rendering
    queue!(stdout, style::SetAttribute(style::Attribute::Reset))?;

    Ok(())
}

/// Render using vt100's built-in diff mechanism.
/// The diff produces escape sequences that transform prev_screen into current screen.
/// We write these directly to stdout with cursor offset.
pub fn render_diff(
    stdout: &mut impl Write,
    prev_screen: &vt100::Screen,
    current_screen: &vt100::Screen,
    offset_x: u16,
    offset_y: u16,
) -> anyhow::Result<()> {
    // For single-pane (offset 0,0), we can use contents_diff directly
    if offset_x == 0 && offset_y == 0 {
        let diff = current_screen.contents_diff(prev_screen);
        stdout.write_all(&diff)?;
        return Ok(());
    }

    // For offset panes, fall back to full render
    // (Multi-pane diff rendering will be added in Phase 4.2)
    let (_, cols) = current_screen.size();
    let (rows, _) = current_screen.size();
    render_full(stdout, current_screen, offset_x, offset_y, cols, rows)?;
    Ok(())
}

/// Render the cursor at the correct position.
pub fn render_cursor(
    stdout: &mut impl Write,
    screen: &vt100::Screen,
    offset_x: u16,
    offset_y: u16,
    visible: bool,
) -> anyhow::Result<()> {
    let (row, col) = screen.cursor_position();
    if visible {
        queue!(
            stdout,
            cursor::MoveTo(offset_x + col, offset_y + row),
            cursor::Show
        )?;
    } else {
        queue!(stdout, cursor::Hide)?;
    }
    Ok(())
}

/// Clear the entire terminal screen.
pub fn clear_screen(stdout: &mut impl Write) -> anyhow::Result<()> {
    queue!(
        stdout,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    Ok(())
}

/// Convert a vt100::Color to a crossterm::style::Color.
fn convert_color(color: vt100::Color) -> style::Color {
    match color {
        vt100::Color::Default => style::Color::Reset,
        vt100::Color::Idx(idx) => match idx {
            0 => style::Color::Black,
            1 => style::Color::DarkRed,
            2 => style::Color::DarkGreen,
            3 => style::Color::DarkYellow,
            4 => style::Color::DarkBlue,
            5 => style::Color::DarkMagenta,
            6 => style::Color::DarkCyan,
            7 => style::Color::Grey,
            8 => style::Color::DarkGrey,
            9 => style::Color::Red,
            10 => style::Color::Green,
            11 => style::Color::Yellow,
            12 => style::Color::Blue,
            13 => style::Color::Magenta,
            14 => style::Color::Cyan,
            15 => style::Color::White,
            n => style::Color::AnsiValue(n),
        },
        vt100::Color::Rgb(r, g, b) => style::Color::Rgb { r, g, b },
    }
}
