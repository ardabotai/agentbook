use crate::app::{App, ChatRole, View};
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
            Constraint::Length(3), // input
            Constraint::Length(1), // status bar
        ])
        .split(frame.area());

    draw_tab_bar(frame, app, chunks[0]);

    // Split main content: left = feed/DMs, right = agent chat
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    match app.view {
        View::Feed => draw_feed(frame, app, main_chunks[0]),
        View::Dms => draw_dms(frame, app, main_chunks[0]),
    }

    draw_agent_chat(frame, app, main_chunks[1]);
    draw_input(frame, app, chunks[2]);
    draw_status_bar(frame, app, chunks[3]);
}

fn draw_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let feed_style = if app.view == View::Feed {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let dm_style = if app.view == View::Dms {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let agent_indicator = if app.agent_connected {
        Span::styled(" agent:on ", Style::default().fg(Color::Green))
    } else {
        Span::styled(" agent:off ", Style::default().fg(Color::Red))
    };

    let tabs = Line::from(vec![
        Span::styled(" [1] Feed ", feed_style),
        Span::raw(" | "),
        Span::styled(" [2] DMs ", dm_style),
        Span::raw(" | "),
        agent_indicator,
        Span::raw("   (Tab to switch, Esc to quit)"),
    ]);
    frame.render_widget(Paragraph::new(tabs), area);
}

fn draw_feed(frame: &mut Frame, app: &App, area: Rect) {
    let messages = app.visible_messages();
    let items: Vec<ListItem> = messages
        .iter()
        .map(|m| {
            let style = if m.acked {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            let from = truncate(&m.from_node_id, 12);
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
    let messages = app.visible_messages();
    let items: Vec<ListItem> = messages
        .iter()
        .map(|m| {
            let from = truncate(&m.from_node_id, 12);
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

fn draw_agent_chat(frame: &mut Frame, app: &App, area: Rect) {
    let mut items: Vec<ListItem> = app
        .chat_history
        .iter()
        .map(|line| {
            let (prefix, style) = match line.role {
                ChatRole::User => ("you: ", Style::default().fg(Color::Cyan)),
                ChatRole::Agent => ("agent: ", Style::default().fg(Color::Green)),
                ChatRole::System => (
                    "* ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::ITALIC),
                ),
            };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
                Span::styled(&line.text, style),
            ]))
        })
        .collect();

    // Show streaming buffer if agent is typing
    if app.agent_typing && !app.agent_buffer.is_empty() {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                "agent: ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&app.agent_buffer, Style::default().fg(Color::Green)),
            Span::styled("_", Style::default().fg(Color::Green)),
        ])));
    } else if app.agent_typing {
        items.push(ListItem::new(Span::styled(
            "agent is thinking...",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Show approval prompt if pending
    if let Some(ref approval) = app.pending_approval {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                "APPROVE? ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}: {}", approval.action, approval.details),
                Style::default().fg(Color::Yellow),
            ),
        ])));
        items.push(ListItem::new(Span::styled(
            "  Press Y to approve, N to deny",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let title = if app.agent_connected {
        " Agent "
    } else {
        " Agent (not connected) "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.pending_approval.is_some() {
        " Y/N to approve | Enter to chat with agent "
    } else {
        match app.view {
            View::Feed => " Chat with agent (Enter to send) ",
            View::Dms => " Chat with agent (Enter to send) ",
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
