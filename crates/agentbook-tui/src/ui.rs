use crate::app::{App, Tab};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(5),    // main content
            Constraint::Length(3), // input (hidden on Terminal tab)
            Constraint::Length(1), // status bar
        ])
        .split(frame.area());

    draw_tab_bar(frame, app, chunks[0]);

    match &app.tab {
        Tab::Feed => {
            draw_feed(frame, app, chunks[1]);
            draw_input(frame, app, chunks[2]);
        }
        Tab::Dms => {
            draw_dms(frame, app, chunks[1]);
            draw_input(frame, app, chunks[2]);
        }
        Tab::Terminal => {
            // Terminal gets the main area + input area combined.
            let term_area = Rect {
                x: chunks[1].x,
                y: chunks[1].y,
                width: chunks[1].width,
                height: chunks[1].height + chunks[2].height,
            };
            draw_terminal(frame, app, term_area);
        }
        Tab::Room(room) => {
            draw_room(frame, app, room, chunks[1]);
            draw_input(frame, app, chunks[2]);
        }
    }

    draw_status_bar(frame, app, chunks[3]);
}

fn draw_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let active = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(Color::DarkGray);
    let activity = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);

    let tab_style = |tab: &Tab| -> Style {
        if app.tab == *tab { active } else { inactive }
    };

    let mut spans = vec![Span::styled(" [1] Terminal", tab_style(&Tab::Terminal))];
    if app.activity_terminal && app.tab != Tab::Terminal {
        spans.push(Span::styled("*", activity));
    }
    spans.push(Span::styled(" | ", inactive));
    spans.push(Span::styled("[2] Feed", tab_style(&Tab::Feed)));
    if app.activity_feed && app.tab != Tab::Feed {
        spans.push(Span::styled("*", activity));
    }
    spans.push(Span::styled(" | ", inactive));
    spans.push(Span::styled("[3] DMs", tab_style(&Tab::Dms)));
    if app.activity_dms && app.tab != Tab::Dms {
        spans.push(Span::styled("*", activity));
    }

    // Dynamic room tabs
    for (i, room) in app.rooms.iter().enumerate() {
        spans.push(Span::styled(" | ", inactive));
        let num = i + 4;
        let is_secure = app.secure_rooms.contains(room);
        let lock = if is_secure { "\u{1f512} " } else { "" };
        let label = format!("[{num}] {lock}#{room}");
        let room_tab = Tab::Room(room.clone());
        spans.push(Span::styled(label, tab_style(&room_tab)));
        let has_activity = app.activity_rooms.get(room).copied().unwrap_or(false);
        if has_activity && app.tab != room_tab {
            spans.push(Span::styled("*", activity));
        }
    }

    if app.prefix_mode {
        spans.push(Span::styled(
            "  [PREFIX]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    spans.push(Span::styled(
        "   Ctrl+Space \u{2192} 1/2/3",
        Style::default().fg(Color::DarkGray),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_feed(frame: &mut Frame, app: &App, area: Rect) {
    let all = app.visible_messages();
    let messages = scroll_window(&all, area.height.saturating_sub(2) as usize, app.current_scroll());
    let items: Vec<ListItem> = messages
        .iter()
        .map(|m| {
            let style = if m.acked {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            let from = display_name(m);
            ListItem::new(Line::from(vec![
                Span::styled(format!("@{from} "), Style::default().fg(Color::Cyan)),
                Span::styled(&m.body, style),
            ]))
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(" Feed ");
    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_dms(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    // Contact list
    let contacts: Vec<ListItem> = app
        .following
        .iter()
        .enumerate()
        .map(|(i, node_id)| {
            let style = if i == app.selected_contact {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Span::styled(truncate(node_id, 16), style))
        })
        .collect();

    let contact_block = Block::default().borders(Borders::ALL).title(" Contacts ");
    let contact_list = List::new(contacts).block(contact_block);
    frame.render_widget(contact_list, chunks[0]);

    // Messages for selected contact
    let all = app.visible_messages();
    let msg_height = chunks[1].height.saturating_sub(2) as usize;
    let messages = scroll_window(&all, msg_height, app.current_scroll());
    let items: Vec<ListItem> = messages
        .iter()
        .map(|m| {
            let from = display_name(m);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{from}: "), Style::default().fg(Color::Cyan)),
                Span::styled(&m.body, Style::default().fg(Color::White)),
            ]))
        })
        .collect();

    let msg_block = Block::default().borders(Borders::ALL).title(" Messages ");
    let msg_list = List::new(items).block(msg_block);
    frame.render_widget(msg_list, chunks[1]);
}

fn draw_terminal(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Terminal ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(ref term) = app.terminal else {
        let hint = Paragraph::new(Line::from(vec![Span::styled(
            "  Press Enter to start shell",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )]));
        frame.render_widget(hint, inner);
        return;
    };

    let screen = term.screen();
    let rows = inner.height as usize;
    let cols = inner.width as usize;

    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut spans: Vec<Span> = Vec::with_capacity(cols);
        for col in 0..cols {
            let cell = screen.cell(row as u16, col as u16);
            let ch = match cell {
                Some(c) => {
                    let s = c.contents();
                    if s.is_empty() { " " } else { s }
                }
                None => " ",
            };

            let style = match cell {
                Some(c) => vt100_style_to_ratatui(c),
                None => Style::default(),
            };

            spans.push(Span::styled(ch, style));
        }
        lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    // Render cursor
    let (cursor_row, cursor_col) = screen.cursor_position();
    let cursor_x = inner.x + cursor_col;
    let cursor_y = inner.y + cursor_row;
    if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn draw_room(frame: &mut Frame, app: &App, room: &str, area: Rect) {
    let all = app.visible_messages();
    let messages = scroll_window(&all, area.height.saturating_sub(2) as usize, app.current_scroll());
    let items: Vec<ListItem> = messages
        .iter()
        .map(|m| {
            if m.message_type == agentbook::protocol::MessageType::RoomJoin {
                ListItem::new(Line::from(Span::styled(
                    format!("  \u{2192} {}", m.body),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )))
            } else {
                let from = display_name(m);
                ListItem::new(Line::from(vec![
                    Span::styled(format!("@{from} "), Style::default().fg(Color::Cyan)),
                    Span::styled(&m.body, Style::default().fg(Color::White)),
                ]))
            }
        })
        .collect();

    let is_secure = app.secure_rooms.contains(room);
    let lock = if is_secure { "\u{1f512}" } else { "" };
    let title = format!(" {lock}#{room} ");
    let block = Block::default().borders(Borders::ALL).title(title);
    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let title = match &app.tab {
        Tab::Feed => " Post to feed (Enter to send) ".to_string(),
        Tab::Dms => " Send DM (Enter to send) ".to_string(),
        Tab::Terminal => String::new(),
        Tab::Room(room) => {
            let lock = if app.secure_rooms.contains(room) {
                "\u{1f512}"
            } else {
                ""
            };
            format!(" {lock}#{room} (140 char limit) ")
        }
    };
    let input = Paragraph::new(app.input.as_str())
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(input, area);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let unread = app.messages.iter().filter(|m| !m.acked).count();
    let status = Line::from(vec![
        Span::styled(
            format!(" {} ", truncate(&app.node_id, 16)),
            Style::default().fg(Color::Green),
        ),
        Span::raw(format!(
            " | {} msgs | {} unread",
            app.messages.len(),
            unread
        )),
        if !app.status_msg.is_empty() {
            Span::styled(
                format!(" | {} ", app.status_msg),
                Style::default().fg(Color::Yellow),
            )
        } else {
            Span::raw("")
        },
    ]);
    frame.render_widget(
        Paragraph::new(status).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

/// Map vt100 cell attributes to ratatui Style.
fn vt100_style_to_ratatui(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();

    style = style.fg(vt100_color_to_ratatui(cell.fgcolor()));
    style = style.bg(vt100_color_to_ratatui(cell.bgcolor()));

    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }

    style
}

fn vt100_color_to_ratatui(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(idx) => Color::Indexed(idx),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Return a window of `height` items from `messages`, shifted up by `offset` from the bottom.
/// offset=0 → show the last `height` items (newest at bottom).
/// offset>0 → scroll up into older messages.
fn scroll_window<'a>(messages: &[&'a agentbook::protocol::InboxEntry], height: usize, offset: usize) -> Vec<&'a agentbook::protocol::InboxEntry> {
    let total = messages.len();
    let clamped_offset = offset.min(total.saturating_sub(1));
    let end = total.saturating_sub(clamped_offset);
    let start = end.saturating_sub(height);
    messages[start..end].to_vec()
}

fn display_name(entry: &agentbook::protocol::InboxEntry) -> String {
    if let Some(u) = &entry.from_username {
        if !u.is_empty() {
            return u.clone();
        }
    }
    truncate(&entry.from_node_id, 12)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
