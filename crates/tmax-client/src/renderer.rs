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
                    // NormalIntensity (SGR 22) clears both bold and dim;
                    // re-emit Dim if it should still be active.
                    if !bold && dim {
                        queue!(stdout, style::SetAttribute(style::Attribute::Dim))?;
                    }
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
                    // NormalIntensity (SGR 22) clears both bold and dim;
                    // re-emit Bold if it should still be active.
                    if !dim && bold {
                        queue!(stdout, style::SetAttribute(style::Attribute::Bold))?;
                    }
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
pub(crate) fn convert_color(color: vt100::Color) -> style::Color {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_color_default() {
        assert_eq!(convert_color(vt100::Color::Default), style::Color::Reset);
    }

    #[test]
    fn convert_color_basic_16() {
        assert_eq!(convert_color(vt100::Color::Idx(0)), style::Color::Black);
        assert_eq!(convert_color(vt100::Color::Idx(1)), style::Color::DarkRed);
        assert_eq!(convert_color(vt100::Color::Idx(7)), style::Color::Grey);
        assert_eq!(convert_color(vt100::Color::Idx(9)), style::Color::Red);
        assert_eq!(convert_color(vt100::Color::Idx(15)), style::Color::White);
    }

    #[test]
    fn convert_color_256_palette() {
        assert_eq!(convert_color(vt100::Color::Idx(16)), style::Color::AnsiValue(16));
        assert_eq!(convert_color(vt100::Color::Idx(231)), style::Color::AnsiValue(231));
        assert_eq!(convert_color(vt100::Color::Idx(255)), style::Color::AnsiValue(255));
    }

    #[test]
    fn convert_color_rgb() {
        assert_eq!(
            convert_color(vt100::Color::Rgb(255, 128, 0)),
            style::Color::Rgb { r: 255, g: 128, b: 0 }
        );
    }

    #[test]
    fn render_full_plain_text() {
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(b"hello");

        let mut buf = Vec::new();
        render_full(&mut buf, parser.screen(), 0, 0, 10, 3).unwrap();

        // Buffer should contain crossterm escape sequences + the text
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("hello"), "output should contain 'hello': {output}");
    }

    #[test]
    fn render_full_respects_dimensions() {
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");

        let mut buf = Vec::new();
        // Only render 3 rows x 10 cols
        render_full(&mut buf, parser.screen(), 0, 0, 10, 3).unwrap();

        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("line1"), "should contain line1");
        assert!(output.contains("line3"), "should contain line3");
        // line4 and line5 should NOT be rendered (height limit = 3)
        assert!(!output.contains("line4"), "should not contain line4");
    }

    #[test]
    fn render_full_colored_text() {
        let mut parser = vt100::Parser::new(3, 20, 0);
        // Red text: ESC[31m
        parser.process(b"\x1b[31mred text\x1b[0m");

        let mut buf = Vec::new();
        render_full(&mut buf, parser.screen(), 0, 0, 20, 3).unwrap();

        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("red text"), "output should contain 'red text'");
        // Should contain a foreground color escape (crossterm format)
        assert!(buf.len() > "red text".len(), "buffer should contain escape sequences");
    }

    #[test]
    fn render_diff_no_change_produces_minimal_output() {
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(b"hello");

        let screen1 = parser.screen().clone();
        // No new input - diff should be minimal
        let screen2 = parser.screen();

        let mut buf = Vec::new();
        render_diff(&mut buf, &screen1, screen2, 0, 0).unwrap();

        // Diff of identical screens should produce very little output
        assert!(buf.len() < 50, "diff of same screen should be small, got {} bytes", buf.len());
    }

    #[test]
    fn render_diff_detects_changes() {
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(b"hello");
        let screen1 = parser.screen().clone();

        parser.process(b" world");
        let screen2 = parser.screen();

        let mut buf = Vec::new();
        render_diff(&mut buf, &screen1, screen2, 0, 0).unwrap();

        // The diff contains escape sequences + changed characters
        // It should be non-empty since the screen changed
        assert!(!buf.is_empty(), "diff should produce output for changed screen");
        // Verify by applying: parse diff output through a fresh parser seeded with screen1
        let mut verify = vt100::Parser::new(3, 10, 0);
        verify.process(b"hello");
        verify.process(&buf);
        assert_eq!(
            verify.screen().contents(),
            screen2.contents(),
            "applying diff to prev screen should produce current screen"
        );
    }

    #[test]
    fn render_cursor_visible() {
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(b"hi");

        let mut buf = Vec::new();
        render_cursor(&mut buf, parser.screen(), 0, 0, true).unwrap();

        // Should contain cursor show sequence
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("\x1b[?25h"), "should show cursor");
    }

    #[test]
    fn render_cursor_hidden() {
        let parser = vt100::Parser::new(3, 10, 0);

        let mut buf = Vec::new();
        render_cursor(&mut buf, parser.screen(), 0, 0, false).unwrap();

        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("\x1b[?25l"), "should hide cursor");
    }

    #[test]
    fn clear_screen_writes_clear_sequence() {
        let mut buf = Vec::new();
        clear_screen(&mut buf).unwrap();

        let output = String::from_utf8_lossy(&buf);
        // Should contain clear all and move to 0,0
        assert!(!buf.is_empty(), "clear_screen should write something");
        assert!(output.contains("\x1b[2J"), "should contain clear sequence");
    }

    #[test]
    fn render_full_empty_cells_produce_spaces() {
        let parser = vt100::Parser::new(2, 5, 0);
        // No input - all cells empty

        let mut buf = Vec::new();
        render_full(&mut buf, parser.screen(), 0, 0, 5, 2).unwrap();

        let output = String::from_utf8_lossy(&buf);
        // Count spaces in output (should have at least 10 = 2 rows x 5 cols)
        let space_count = output.chars().filter(|&c| c == ' ').count();
        assert!(space_count >= 10, "empty screen should render as spaces, got {space_count}");
    }
}
