use crate::app::{App, SidekickRole, Tab, TerminalSplit};
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

pub const HEADER_HEIGHT: u16 = 6;
const CONTROLS_PREFIX: &str = " Controls (click or Ctrl+Space): ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabClickTarget {
    TerminalWindow(usize),
    SocialTab(Tab),
    Control(ControlAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    NewTab,
    NextTab,
    PrevTab,
    CloseTab,
    ToggleSidekick,
    ToggleSound,
    Quit,
    SplitVertical,
    SplitHorizontal,
    NextPane,
    ClosePane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitModalClickTarget {
    Confirm,
    Cancel,
}

pub fn draw(frame: &mut Frame, app: &App) {
    // Force full redraw every frame to avoid stale glyphs when switching tabs or resizing.
    frame.render_widget(Clear, frame.area());

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HEADER_HEIGHT), // bordered header + 3 rows
            Constraint::Min(5),                // main content
            Constraint::Length(3),             // input (hidden on Terminal tab)
            Constraint::Length(1),             // status bar
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
            let full_terminal_area = Rect {
                x: chunks[1].x,
                y: chunks[1].y,
                width: chunks[1].width,
                height: chunks[1].height + chunks[2].height,
            };
            let (term_area, sidekick_area) =
                terminal_main_and_sidekick_areas(full_terminal_area, app.auto_agent.enabled);
            draw_terminal(frame, app, term_area);
            if let Some(sidekick_area) = sidekick_area {
                draw_sidekick(frame, app, sidekick_area);
            }
        }
        Tab::Room(room) => {
            draw_room(frame, app, room, chunks[1]);
            draw_input(frame, app, chunks[2]);
        }
    }

    draw_status_bar(frame, app, chunks[3]);
    if app.quit_confirm {
        draw_quit_confirm_modal(frame, frame.area());
    }
}

fn draw_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let active = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(Color::DarkGray);
    let activity = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let row_active = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let row_inactive = Style::default().fg(Color::Gray);
    let waiting_input = Style::default()
        .fg(Color::LightRed)
        .add_modifier(Modifier::BOLD);
    let controls_label = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let controls_item_active = Style::default().fg(Color::White);
    let controls_item_inactive = Style::default().fg(Color::DarkGray);
    let terminal_row_is_active = app.tab == Tab::Terminal;
    let social_row_is_active = !terminal_row_is_active;
    let controls_row_is_active = terminal_row_is_active;
    let row_label_style = |active_row: bool| {
        if active_row { row_active } else { row_inactive }
    };

    let tab_style = |tab: &Tab| -> Style { if app.tab == *tab { active } else { inactive } };

    let mut top = vec![Span::styled(
        " Terminal Tabs: ",
        row_label_style(terminal_row_is_active),
    )];
    if app.terminal_window_tabs.is_empty() {
        top.push(Span::styled("[1] shell", tab_style(&Tab::Terminal)));
    } else {
        for (i, t) in app.terminal_window_tabs.iter().enumerate() {
            if i > 0 {
                top.push(Span::styled(" | ", inactive));
            }
            let window_idx = app.terminal_window_indices.get(i).copied().unwrap_or(i);
            let style = if app.tab == Tab::Terminal && i == app.active_terminal_window {
                if app.terminal_waiting_input_windows.contains(&window_idx) {
                    active.add_modifier(Modifier::UNDERLINED)
                } else {
                    active
                }
            } else if app.terminal_waiting_input_windows.contains(&window_idx) {
                waiting_input
            } else {
                inactive
            };
            top.push(Span::styled(format!("[T{}] {t}", i + 1), style));
        }
    }
    if app.activity_terminal && app.tab != Tab::Terminal {
        top.push(Span::styled("*", activity));
    }

    if app.prefix_mode {
        top.push(Span::styled(
            "  [PREFIX]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let mut rooms = vec![Span::styled(
        " Social: ",
        row_label_style(social_row_is_active),
    )];
    rooms.push(Span::styled("[2] Feed", tab_style(&Tab::Feed)));
    if app.activity_feed && app.tab != Tab::Feed {
        rooms.push(Span::styled("*", activity));
    }
    rooms.push(Span::styled(" | ", inactive));
    rooms.push(Span::styled("[3] DMs", tab_style(&Tab::Dms)));
    if app.activity_dms && app.tab != Tab::Dms {
        rooms.push(Span::styled("*", activity));
    }
    rooms.push(Span::styled(" | Rooms: ", inactive));
    if app.rooms.is_empty() {
        rooms.push(Span::styled("(none)", inactive));
    } else {
        for (i, room) in app.rooms.iter().enumerate() {
            if i > 0 {
                rooms.push(Span::styled(" | ", inactive));
            }
            let num = i + 4;
            let is_secure = app.secure_rooms.contains(room);
            let lock = if is_secure { "\u{1f512} " } else { "" };
            let label = format!("[{num}] {lock}#{room}");
            let room_tab = Tab::Room(room.clone());
            rooms.push(Span::styled(label, tab_style(&room_tab)));
            let has_activity = app.activity_rooms.get(room).copied().unwrap_or(false);
            if has_activity && app.tab != room_tab {
                rooms.push(Span::styled("*", activity));
            }
        }
    }

    let mut controls = vec![Span::styled(CONTROLS_PREFIX, controls_label)];
    let controls_style = if controls_row_is_active {
        controls_item_active
    } else {
        controls_item_inactive
    };
    let items = control_items();
    for (i, (_, label)) in items.iter().enumerate() {
        if i > 0 {
            controls.push(Span::styled(" | ", inactive));
        }
        controls.push(Span::styled(*label, controls_style));
    }

    let lines = vec![Line::from(top), Line::from(rooms), Line::from(controls)];
    let block = Block::default().borders(Borders::ALL).title(" Navigation ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Hit-test the navigation rows inside the bordered header section.
///
/// `column` and `row` are absolute terminal coordinates from crossterm.
pub fn tab_click_target(
    app: &App,
    column: u16,
    row: u16,
    viewport: Rect,
) -> Option<TabClickTarget> {
    let header = Rect {
        x: viewport.x,
        y: viewport.y,
        width: viewport.width,
        height: HEADER_HEIGHT.min(viewport.height),
    };
    let inner = Rect {
        x: header.x.saturating_add(1),
        y: header.y.saturating_add(1),
        width: header.width.saturating_sub(2),
        height: header.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    if row < inner.y || row >= inner.y.saturating_add(inner.height) {
        return None;
    }

    let top_row = inner.y;
    let social_row = inner.y.saturating_add(1);
    let controls_start_row = inner.y.saturating_add(2);

    if row == top_row {
        let mut x: u16 = inner
            .x
            .saturating_add(" Terminal Tabs: ".chars().count() as u16);
        if app.terminal_window_tabs.is_empty() {
            let label = "[1] shell";
            if in_range(column, x, label) {
                return Some(TabClickTarget::TerminalWindow(0));
            }
            return None;
        }

        for (i, t) in app.terminal_window_tabs.iter().enumerate() {
            if i > 0 {
                x = x.saturating_add(" | ".chars().count() as u16);
            }
            let label = format!("[T{}] {t}", i + 1);
            if in_range(column, x, &label) {
                return Some(TabClickTarget::TerminalWindow(i));
            }
            x = x.saturating_add(label.chars().count() as u16);
        }
        return None;
    }

    if row == social_row {
        // social tabs
        let mut x: u16 = inner.x.saturating_add(" Social: ".chars().count() as u16);
        let feed = "[2] Feed";
        if in_range(column, x, feed) {
            return Some(TabClickTarget::SocialTab(Tab::Feed));
        }
        x = x.saturating_add(feed.chars().count() as u16);
        if app.activity_feed {
            if column == x {
                return Some(TabClickTarget::SocialTab(Tab::Feed));
            }
            x = x.saturating_add(1);
        }

        x = x.saturating_add(" | ".chars().count() as u16);
        let dms = "[3] DMs";
        if in_range(column, x, dms) {
            return Some(TabClickTarget::SocialTab(Tab::Dms));
        }
        x = x.saturating_add(dms.chars().count() as u16);
        if app.activity_dms {
            if column == x {
                return Some(TabClickTarget::SocialTab(Tab::Dms));
            }
            x = x.saturating_add(1);
        }

        x = x.saturating_add(" | Rooms: ".chars().count() as u16);
        for (i, room) in app.rooms.iter().enumerate() {
            if i > 0 {
                x = x.saturating_add(" | ".chars().count() as u16);
            }
            let num = i + 4;
            let is_secure = app.secure_rooms.contains(room);
            let lock = if is_secure { "\u{1f512} " } else { "" };
            let label = format!("[{num}] {lock}#{room}");
            if in_range(column, x, &label) {
                return Some(TabClickTarget::SocialTab(Tab::Room(room.clone())));
            }
            x = x.saturating_add(label.chars().count() as u16);
            let has_activity = app.activity_rooms.get(room).copied().unwrap_or(false);
            if has_activity {
                if column == x {
                    return Some(TabClickTarget::SocialTab(Tab::Room(room.clone())));
                }
                x = x.saturating_add(1);
            }
        }

        return None;
    }

    if row < controls_start_row {
        return None;
    }
    // Controls can wrap; hit-test against wrapped rows using the same token order.
    let target_visual_row = row.saturating_sub(controls_start_row);
    let mut visual_row = 0u16;
    let mut x: u16 = 0;
    let max_width = inner.width;

    let mut tokens: Vec<(Option<ControlAction>, &'static str)> = Vec::new();
    tokens.push((None, CONTROLS_PREFIX));
    for (i, (action, label)) in control_items().iter().enumerate() {
        if i > 0 {
            tokens.push((None, " | "));
        }
        tokens.push((Some(*action), *label));
    }

    for (action, text) in tokens {
        let len = text.chars().count() as u16;
        if x > 0 && x.saturating_add(len) > max_width {
            visual_row = visual_row.saturating_add(1);
            x = 0;
        }
        if let Some(action) = action {
            let abs_x = inner.x.saturating_add(x);
            if visual_row == target_visual_row && in_range(column, abs_x, text) {
                return Some(TabClickTarget::Control(action));
            }
        }
        x = x.saturating_add(len);
    }

    None
}

fn control_items() -> &'static [(ControlAction, &'static str)] {
    &[
        (ControlAction::NewTab, "[C] New Tab"),
        (ControlAction::NextTab, "[N] Next Tab"),
        (ControlAction::PrevTab, "[P] Prev Tab"),
        (ControlAction::CloseTab, "[W] Close Tab"),
        (ControlAction::ToggleSidekick, "[A] Sidekick"),
        (ControlAction::ToggleSound, "[S] Sound"),
        (ControlAction::Quit, "[Q] Quit"),
        (ControlAction::SplitVertical, "[%] Split V"),
        (ControlAction::SplitHorizontal, "[\"] Split H"),
        (ControlAction::NextPane, "[O] Next Pane"),
        (ControlAction::ClosePane, "[X] Close Pane"),
    ]
}

fn in_range(column: u16, start: u16, text: &str) -> bool {
    let end = start.saturating_add(text.chars().count() as u16);
    column >= start && column < end
}

pub fn quit_modal_click_target(
    column: u16,
    row: u16,
    viewport: Rect,
) -> Option<QuitModalClickTarget> {
    let layout = quit_modal_layout(viewport);
    if row != layout.buttons_row {
        return None;
    }
    if column >= layout.yes_start && column < layout.yes_end {
        return Some(QuitModalClickTarget::Confirm);
    }
    if column >= layout.no_start && column < layout.no_end {
        return Some(QuitModalClickTarget::Cancel);
    }
    None
}

fn draw_quit_confirm_modal(frame: &mut Frame, viewport: Rect) {
    let layout = quit_modal_layout(viewport);
    frame.render_widget(Clear, layout.modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Confirm Exit ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(layout.modal);
    frame.render_widget(block, layout.modal);

    let yes = "[ Yes (Y) ]";
    let no = "[ No (N) ]";
    let text = vec![
        Line::from("Are you sure you want to close agentbook?"),
        Line::from("Press Y/N (or Ctrl+Space then Y/N), or click a button."),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                yes,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                no,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    let paragraph = Paragraph::new(text)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

#[derive(Debug, Clone, Copy)]
struct QuitModalLayout {
    modal: Rect,
    buttons_row: u16,
    yes_start: u16,
    yes_end: u16,
    no_start: u16,
    no_end: u16,
}

fn quit_modal_layout(viewport: Rect) -> QuitModalLayout {
    let width = viewport.width.saturating_sub(8).min(70).max(44);
    let height = 8;
    let x = viewport.x + (viewport.width.saturating_sub(width)) / 2;
    let y = viewport.y + (viewport.height.saturating_sub(height)) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };
    let inner = Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        width: modal.width.saturating_sub(2),
        height: modal.height.saturating_sub(2),
    };
    let yes = "[ Yes (Y) ]";
    let no = "[ No (N) ]";
    let buttons_total = yes.chars().count() as u16 + 3 + no.chars().count() as u16;
    let buttons_row = inner.y + 3;
    let buttons_start = inner.x + (inner.width.saturating_sub(buttons_total)) / 2;
    let yes_start = buttons_start;
    let yes_end = yes_start + yes.chars().count() as u16;
    let no_start = yes_end + 3;
    let no_end = no_start + no.chars().count() as u16;
    QuitModalLayout {
        modal,
        buttons_row,
        yes_start,
        yes_end,
        no_start,
        no_end,
    }
}

fn draw_feed(frame: &mut Frame, app: &App, area: Rect) {
    let all = app.visible_messages();
    let messages = scroll_window(
        &all,
        area.height.saturating_sub(2) as usize,
        app.current_scroll(),
    );
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
    if app.terminals.is_empty() {
        let block = Block::default().borders(Borders::ALL).title(" Terminal ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let hint = Paragraph::new(Line::from(vec![Span::styled(
            "  Press Enter to start shell",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )]));
        frame.render_widget(hint, inner);
        return;
    }

    let pane_areas = terminal_pane_areas(area, app.terminals.len(), app.terminal_split);
    for (idx, (pane_area, term)) in pane_areas.iter().zip(app.terminals.iter()).enumerate() {
        draw_terminal_pane(frame, *pane_area, term, idx == app.active_terminal, idx + 1);
    }
}

pub fn terminal_main_and_sidekick_areas(
    area: Rect,
    sidekick_enabled: bool,
) -> (Rect, Option<Rect>) {
    if !sidekick_enabled || area.height < 8 {
        return (area, None);
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(67), Constraint::Percentage(33)])
        .split(area);
    (chunks[0], Some(chunks[1]))
}

pub fn sidekick_area_for_viewport(viewport: Rect, sidekick_enabled: bool) -> Option<Rect> {
    let full_terminal_area = Rect {
        x: viewport.x,
        y: viewport.y + HEADER_HEIGHT,
        width: viewport.width,
        // total minus header section and status bar.
        height: viewport.height.saturating_sub(HEADER_HEIGHT + 1),
    };
    let (_, sidekick) = terminal_main_and_sidekick_areas(full_terminal_area, sidekick_enabled);
    sidekick
}

fn draw_sidekick(frame: &mut Frame, app: &App, area: Rect) {
    let title = format!(
        " Sidekick {} ({}) ",
        app.auto_agent.mode.label(),
        if app.auto_agent.chat_focus {
            "chat"
        } else {
            "observe"
        }
    );
    let outer = Block::default().borders(Borders::ALL).title(title);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    if inner.height < 4 || inner.width < 10 {
        return;
    }
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(inner);

    let mut lines = Vec::new();
    for msg in &app.auto_agent.chat_history {
        let (prefix, style) = match msg.role {
            SidekickRole::User => ("you", Style::default().fg(Color::Cyan)),
            SidekickRole::Assistant => ("pi", Style::default().fg(Color::White)),
            SidekickRole::System => ("sys", Style::default().fg(Color::DarkGray)),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{prefix}: "), style.add_modifier(Modifier::BOLD)),
            Span::raw(msg.content.as_str()),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Sidekick ready. Ctrl+Space A to toggle, Ctrl+Space I to focus chat.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    if app.auto_agent.awaiting_api_key {
        let auth_msg = if crate::automation::has_arda_login() {
            "Arda auth detected but inference failed. Run `agentbook login` to re-authenticate."
        } else {
            "Run `agentbook login` to authenticate, or paste an Anthropic API key below."
        };
        lines.push(Line::from(vec![
            Span::styled(
                "auth: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(auth_msg, Style::default().fg(Color::Yellow)),
        ]));
        if let Some(err) = app.auto_agent.auth_error.as_deref() {
            lines.push(Line::from(vec![
                Span::styled("error: ", Style::default().fg(Color::Red)),
                Span::styled(err, Style::default().fg(Color::DarkGray)),
            ]));
        }
    }
    if app.auto_agent.awaiting_user_input {
        lines.push(Line::from(vec![
            Span::styled(
                "decision: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Sidekick paused. User input required for a major architecture decision.",
                Style::default().fg(Color::Yellow),
            ),
        ]));
        if let Some(question) = app.auto_agent.pending_user_question.as_deref() {
            lines.push(Line::from(vec![
                Span::styled("question: ", Style::default().fg(Color::Cyan)),
                Span::raw(question),
            ]));
        }
    }
    if !app.auto_agent.last_summary.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("summary: ", Style::default().fg(Color::Green)),
            Span::styled(
                app.auto_agent.last_summary.as_str(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    let view_height = sections[0].height as usize;
    let total = lines.len();
    let clamped_offset = app.auto_agent.chat_scroll.min(total.saturating_sub(1));
    let end = total.saturating_sub(clamped_offset);
    let start = end.saturating_sub(view_height);
    let visible = lines[start..end].to_vec();
    frame.render_widget(
        Paragraph::new(visible).wrap(Wrap { trim: false }),
        sections[0],
    );

    let input_title = if app.auto_agent.awaiting_api_key {
        " API Key (Enter to save, or run `agentbook login`) "
    } else if app.auto_agent.awaiting_user_input {
        " Decision Input (answer Sidekick and press Enter) "
    } else if app.auto_agent.chat_focus {
        " Prompt (Enter send, Esc exit chat focus) "
    } else {
        " Prompt (Ctrl+Space I to focus chat input) "
    };
    let input_text = if app.auto_agent.awaiting_api_key {
        if app.auto_agent.chat_input.is_empty() {
            "".to_string()
        } else {
            "*".repeat(app.auto_agent.chat_input.chars().count())
        }
    } else {
        app.auto_agent.chat_input.clone()
    };
    let input = Paragraph::new(input_text)
        .block(Block::default().borders(Borders::ALL).title(input_title))
        .wrap(Wrap { trim: false });
    frame.render_widget(input, sections[1]);

    if app.auto_agent.chat_focus {
        let cursor_x = sections[1]
            .x
            .saturating_add(1)
            .saturating_add(app.auto_agent.chat_input.chars().count() as u16)
            .min(sections[1].x + sections[1].width.saturating_sub(2));
        let cursor_y = sections[1].y.saturating_add(1);
        if cursor_x < sections[1].x + sections[1].width
            && cursor_y < sections[1].y + sections[1].height
        {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn draw_terminal_pane(
    frame: &mut Frame,
    area: Rect,
    term: &crate::terminal::TerminalEmulator,
    active: bool,
    pane_number: usize,
) {
    let scrolled = term.is_scrolled_back();
    let title = if scrolled {
        format!(" Terminal {pane_number} (scrollback) ")
    } else {
        format!(" Terminal {pane_number} ")
    };
    let mut block = Block::default().borders(Borders::ALL).title(title);
    if active {
        block = block.border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Clear, inner);

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

    frame.render_widget(Paragraph::new(lines), inner);

    // Only render cursor for active pane at live view.
    if active && !scrolled {
        let (cursor_row, cursor_col) = screen.cursor_position();
        let cursor_x = inner.x + cursor_col;
        let cursor_y = inner.y + cursor_row;
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn draw_room(frame: &mut Frame, app: &App, room: &str, area: Rect) {
    let all = app.visible_messages();
    let messages = scroll_window(
        &all,
        area.height.saturating_sub(2) as usize,
        app.current_scroll(),
    );
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

    // Identity: @username / 0x1a2b...3c4d
    let identity = match &app.username {
        Some(name) => format!(" @{name} / {} ", truncate(&app.node_id, 12)),
        None => format!(" {} ", truncate(&app.node_id, 16)),
    };

    let mut spans = vec![Span::styled(identity, Style::default().fg(Color::Green))];

    if unread > 0 {
        spans.push(Span::styled(
            format!(" | {unread} unread"),
            Style::default().fg(Color::Yellow),
        ));
    }

    if app.auto_agent.enabled {
        spans.push(Span::styled(
            format!(" | SIDEKICK:{} ", app.auto_agent.mode.label()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    if !app.status_msg.is_empty() {
        spans.push(Span::styled(
            format!(" | {} ", app.status_msg),
            Style::default().fg(Color::Yellow),
        ));
    }

    let status = Line::from(spans);
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
fn scroll_window<'a>(
    messages: &[&'a agentbook::protocol::InboxEntry],
    height: usize,
    offset: usize,
) -> Vec<&'a agentbook::protocol::InboxEntry> {
    let total = messages.len();
    let clamped_offset = offset.min(total.saturating_sub(1));
    let end = total.saturating_sub(clamped_offset);
    let start = end.saturating_sub(height);
    messages[start..end].to_vec()
}

fn display_name(entry: &agentbook::protocol::InboxEntry) -> String {
    if let Some(u) = &entry.from_username
        && !u.is_empty()
    {
        return u.clone();
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

/// Compute pane rectangles for terminal split rendering/resizing.
pub fn terminal_pane_areas(area: Rect, pane_count: usize, split: TerminalSplit) -> Vec<Rect> {
    if pane_count == 0 {
        return Vec::new();
    }
    if pane_count == 1 || split == TerminalSplit::Single {
        return vec![area];
    }
    let direction = match split {
        TerminalSplit::Vertical => Direction::Horizontal,
        TerminalSplit::Horizontal => Direction::Vertical,
        TerminalSplit::Single => Direction::Vertical,
    };
    let constraints = (0..pane_count)
        .map(|_| Constraint::Ratio(1, pane_count as u32))
        .collect::<Vec<_>>();
    Layout::default()
        .direction(direction)
        .constraints(constraints)
        .split(area)
        .iter()
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_top_row_hits_terminal_window_tab() {
        let mut app = App::new("me".to_string());
        app.terminal_window_tabs = vec!["1 main".to_string(), "2 logs".to_string()];
        app.active_terminal_window = 0;
        app.tab = Tab::Terminal;
        let inner_x = 1u16;

        let prefix = " Terminal Tabs: ".chars().count() as u16;
        let first = "[T1] 1 main".chars().count() as u16;
        let sep = " | ".chars().count() as u16;
        let col = inner_x + prefix + first + sep;
        let viewport = Rect::new(0, 0, 120, 40);

        assert_eq!(
            tab_click_target(&app, col, 1, viewport),
            Some(TabClickTarget::TerminalWindow(1))
        );
    }

    #[test]
    fn click_second_row_hits_social_tabs() {
        let mut app = App::new("me".to_string());
        app.rooms = vec!["general".to_string()];
        let viewport = Rect::new(0, 0, 120, 40);
        let inner_x = 1u16;

        // Feed label starts right after " Social: ".
        let prefix = " Social: ".chars().count() as u16;
        assert_eq!(
            tab_click_target(&app, inner_x + prefix + 1, 2, viewport),
            Some(TabClickTarget::SocialTab(Tab::Feed))
        );

        let feed = "[2] Feed".chars().count() as u16;
        let sep = " | ".chars().count() as u16;
        let dms = "[3] DMs".chars().count() as u16;
        let rooms_prefix = " | Rooms: ".chars().count() as u16;
        let room_start = inner_x + prefix + feed + sep + dms + rooms_prefix;
        assert_eq!(
            tab_click_target(&app, room_start, 2, viewport),
            Some(TabClickTarget::SocialTab(Tab::Room("general".to_string())))
        );
    }

    #[test]
    fn click_third_row_hits_controls() {
        let app = App::new("me".to_string());
        let viewport = Rect::new(0, 0, 120, 40);
        let prefix = CONTROLS_PREFIX.chars().count() as u16;
        assert_eq!(
            tab_click_target(&app, prefix + 1, 3, viewport),
            Some(TabClickTarget::Control(ControlAction::NewTab))
        );

        let new_tab = "[C] New Tab".chars().count() as u16;
        let sep = " | ".chars().count() as u16;
        let next_tab_start = prefix + new_tab + sep;
        assert_eq!(
            tab_click_target(&app, next_tab_start + 1, 3, viewport),
            Some(TabClickTarget::Control(ControlAction::NextTab))
        );

        let prev_tab = "[P] Prev Tab".chars().count() as u16;
        let close_tab = "[W] Close Tab".chars().count() as u16;
        let sidekick = "[A] Sidekick".chars().count() as u16;
        let sound = "[S] Sound".chars().count() as u16;
        let quit_start = next_tab_start
            + "[N] Next Tab".chars().count() as u16
            + sep
            + prev_tab
            + sep
            + close_tab
            + sep
            + sidekick
            + sep
            + sound
            + sep;
        if let Some(hit) = tab_click_target(&app, quit_start + 1, 3, viewport) {
            assert_eq!(hit, TabClickTarget::Control(ControlAction::Quit));
        } else {
            let mut found = None;
            for row in viewport.y..viewport.y + viewport.height {
                for col in viewport.x..viewport.x + viewport.width {
                    if tab_click_target(&app, col, row, viewport)
                        == Some(TabClickTarget::Control(ControlAction::Quit))
                    {
                        found = Some((col, row));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            assert!(found.is_some(), "Quit control should be clickable");
        }
    }

    #[test]
    fn wrapped_controls_row_still_clickable() {
        let app = App::new("me".to_string());
        let viewport = Rect::new(0, 0, 72, 20);
        let mut found = None;
        for row in viewport.y..viewport.y + viewport.height {
            for col in viewport.x..viewport.x + viewport.width {
                if tab_click_target(&app, col, row, viewport)
                    == Some(TabClickTarget::Control(ControlAction::Quit))
                {
                    found = Some((col, row));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (_, row) = found.expect("Quit control should be clickable");
        assert!(
            row >= 4,
            "Quit should wrap onto a lower controls row on narrow widths"
        );
    }

    #[test]
    fn quit_modal_click_targets_buttons() {
        let viewport = Rect::new(0, 0, 120, 40);
        let mut seen_confirm = None;
        let mut seen_cancel = None;
        for row in viewport.y..viewport.y + viewport.height {
            for col in viewport.x..viewport.x + viewport.width {
                match quit_modal_click_target(col, row, viewport) {
                    Some(QuitModalClickTarget::Confirm) if seen_confirm.is_none() => {
                        seen_confirm = Some((col, row));
                    }
                    Some(QuitModalClickTarget::Cancel) if seen_cancel.is_none() => {
                        seen_cancel = Some((col, row));
                    }
                    _ => {}
                }
            }
        }
        assert!(seen_confirm.is_some(), "confirm button should be clickable");
        assert!(seen_cancel.is_some(), "cancel button should be clickable");
    }
}
