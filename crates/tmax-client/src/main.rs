use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use crossterm::cursor::{MoveTo, Show};
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode, size,
};
use crossterm::{ExecutableCommand, QueueableCommand};
use futures_util::{SinkExt, StreamExt};
use nix::unistd::Uid;
use regex::Regex;
use serde_json::Value;
use std::cmp::Ordering;
use std::io::{Write, stdout};
use std::path::PathBuf;
use std::thread;
use tmax_protocol::{AttachMode, Event, MAX_JSON_LINE_BYTES, Request, Response, SessionSummary};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

mod layout;
mod vt;

use layout::{PaneLayout, Rect, SplitAxis};
use vt::VtScreen;

#[derive(Debug)]
struct Args {
    session_id: String,
    socket_path: PathBuf,
    view: bool,
    headless_smoke: bool,
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Debug)]
enum PrefixCmd {
    NewWindow,
    NextWindow,
    PrevWindow,
    SelectWindow(usize),
    SplitHorizontal,
    SplitVertical,
    MoveFocus(Direction),
    GrowPane,
    ShrinkPane,
    Detach,
    SearchPrompt,
    MarkerJump,
    WorktreeSessions,
    ToggleHelp,
    EnterScrollMode,
    Cancel,
}

#[derive(Debug)]
enum InputMsg {
    Bytes(Vec<u8>),
    PrefixArmed,
    PrefixCommand(PrefixCmd),
    SearchDraft(String),
    SearchCommit(String),
    Scroll(i32),
    ExitScrollMode,
    Resize(u16, u16),
    Quit,
}

#[derive(Debug)]
struct PaneMetadata {
    label: Option<String>,
    git_branch: Option<String>,
    git_dirty: Option<bool>,
    git_repo_root: Option<PathBuf>,
    sandboxed: bool,
}

impl From<SessionSummary> for PaneMetadata {
    fn from(value: SessionSummary) -> Self {
        Self {
            label: value.label,
            git_branch: value.git_branch,
            git_dirty: value.git_dirty,
            git_repo_root: value.git_repo_root,
            sandboxed: value.sandboxed,
        }
    }
}

#[derive(Debug)]
struct WindowState {
    layout: PaneLayout,
    active_pane: String,
}

impl WindowState {
    fn new() -> Self {
        let layout = PaneLayout::new();
        let active_pane = layout.root_id().to_string();
        Self {
            layout,
            active_pane,
        }
    }
}

#[derive(Debug, Clone)]
struct MarkerEntry {
    name: String,
    seq: u64,
}

#[derive(Debug)]
struct Scrollback {
    lines: Vec<String>,
    current_line: String,
    seq_line_map: Vec<(u64, usize)>,
    max_lines: usize,
}

impl Scrollback {
    fn new(max_lines: usize) -> Self {
        Self {
            lines: Vec::new(),
            current_line: String::new(),
            seq_line_map: Vec::new(),
            max_lines: max_lines.max(1),
        }
    }

    fn ingest_output(&mut self, seq: u64, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        for ch in text.chars() {
            match ch {
                '\n' => {
                    self.push_completed_line();
                }
                '\r' => {
                    self.current_line.clear();
                }
                _ => self.current_line.push(ch),
            }
        }

        if self.current_line.len() > 4096 {
            self.current_line.truncate(4096);
        }

        self.record_seq(seq);
    }

    fn load_snapshot(&mut self, seq: u64, lines: &[String]) {
        self.lines = lines.to_vec();
        self.current_line.clear();
        self.seq_line_map.clear();
        self.trim_excess();
        if self.line_count() > 0 {
            self.seq_line_map
                .push((seq, self.line_count().saturating_sub(1)));
        }
    }

    fn line_count(&self) -> usize {
        self.lines.len() + usize::from(!self.current_line.is_empty())
    }

    fn window_lines(&self, rows: usize, offset_from_bottom: usize) -> Vec<String> {
        let mut all = self.lines.clone();
        if !self.current_line.is_empty() {
            all.push(self.current_line.clone());
        }

        if all.is_empty() {
            return Vec::new();
        }

        let end = all.len().saturating_sub(offset_from_bottom.min(all.len()));
        let start = end.saturating_sub(rows);
        all[start..end].to_vec()
    }

    fn line_for_seq(&self, seq: u64) -> Option<usize> {
        let idx = self.seq_line_map.partition_point(|(s, _)| *s <= seq);
        if idx == 0 {
            return None;
        }
        let line_idx = self.seq_line_map[idx - 1].1;
        Some(line_idx.min(self.line_count().saturating_sub(1)))
    }

    fn push_completed_line(&mut self) {
        self.lines.push(std::mem::take(&mut self.current_line));
        self.trim_excess();
    }

    fn trim_excess(&mut self) {
        if self.lines.len() <= self.max_lines {
            return;
        }

        let drop_n = self.lines.len() - self.max_lines;
        self.lines.drain(0..drop_n);

        self.seq_line_map
            .retain(|(_, line_idx)| *line_idx >= drop_n);
        for (_, line_idx) in &mut self.seq_line_map {
            *line_idx -= drop_n;
        }
    }

    fn record_seq(&mut self, seq: u64) {
        if self.line_count() == 0 {
            return;
        }
        let line_idx = self.line_count().saturating_sub(1);
        self.seq_line_map.push((seq, line_idx));
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        stdout().execute(EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = out.execute(Show);
        let _ = out.execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse()?;

    let stream = UnixStream::connect(&args.socket_path)
        .await
        .with_context(|| format!("failed to connect {}", args.socket_path.display()))?;
    let (read_half, write_half) = stream.into_split();
    let mut reader = FramedRead::new(
        read_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );
    let mut writer = FramedWrite::new(
        write_half,
        LinesCodec::new_with_max_length(MAX_JSON_LINE_BYTES),
    );

    read_hello(&mut reader).await?;
    let summary = request_session_summary(&mut writer, &mut reader, &args.session_id).await?;
    let metadata: PaneMetadata = summary.into();

    let mode = if args.view {
        AttachMode::View
    } else {
        AttachMode::Edit
    };

    send_request(
        &mut writer,
        &Request::Attach {
            session_id: args.session_id.clone(),
            mode,
            last_seq_seen: None,
        },
    )
    .await?;
    let attachment_id = wait_for_attachment(&mut reader).await?;

    if args.headless_smoke {
        let _ = send_request(
            &mut writer,
            &Request::Detach {
                attachment_id: attachment_id.clone(),
            },
        )
        .await;
        return Ok(());
    }

    let _terminal = TerminalGuard::enter()?;
    let mut out = stdout();
    let (mut term_cols, mut term_rows) = size().unwrap_or((80, 24));

    let mut windows = vec![WindowState::new()];
    let mut active_window = 0usize;

    let (pty_cols, pty_rows) = active_pane_inner_size(
        &windows[active_window].layout,
        &windows[active_window].active_pane,
        term_cols,
        term_rows,
    );
    let mut vt = VtScreen::new(usize::from(pty_cols), usize::from(pty_rows));
    if mode == AttachMode::Edit {
        let _ = send_request(
            &mut writer,
            &Request::Resize {
                session_id: args.session_id.clone(),
                cols: pty_cols,
                rows: pty_rows,
            },
        )
        .await;
    }

    let mut scrollback = Scrollback::new(20_000);
    let mut scroll_mode = false;
    let mut scroll_offset = 0usize;

    let mut status = None::<String>;
    let mut help_overlay = false;

    let mut search_pattern = None::<String>;
    let mut search_regex = None::<Regex>;

    let mut markers = Vec::<MarkerEntry>::new();
    let mut marker_cursor = 0usize;
    let mut pending_marker_list = false;
    let mut pending_worktree_list = false;

    render_screen(
        &mut out,
        &args.session_id,
        mode,
        &metadata,
        &windows[active_window],
        active_window,
        windows.len(),
        &vt,
        &scrollback,
        scroll_mode,
        scroll_offset,
        search_regex.as_ref(),
        search_pattern.as_deref(),
        term_cols,
        term_rows,
        help_overlay,
        status.as_deref(),
    )?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<InputMsg>();
    let input_handle = spawn_input_thread(input_tx);
    let mut done = false;

    while !done {
        tokio::select! {
            maybe_line = reader.next() => {
                let Some(line) = maybe_line else {
                    break;
                };
                let line = line?;
                let resp: Response = serde_json::from_str(&line)?;
                match resp {
                    Response::Event { event } => {
                        match *event {
                            Event::Output { seq, data_b64, .. } => {
                                let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
                                vt.feed(&bytes);
                                scrollback.ingest_output(seq, &bytes);
                                if !scroll_mode {
                                    scroll_offset = 0;
                                }
                            }
                            Event::Snapshot {
                                seq,
                                cols,
                                rows,
                                lines,
                                ..
                            } => {
                                vt.load_snapshot(cols, rows, &lines);
                                scrollback.load_snapshot(seq, &lines);
                                if !scroll_mode {
                                    scroll_offset = 0;
                                }
                            }
                            Event::SessionExited { .. } => {
                                status = Some("Session exited. Press Ctrl-Q to close.".to_string());
                            }
                            Event::MarkerInserted { name, seq, .. } => {
                                markers.push(MarkerEntry { name, seq });
                            }
                            _ => {}
                        }
                    }
                    Response::Ok { data } => {
                        if pending_marker_list {
                            pending_marker_list = false;
                            markers = parse_markers(data);
                            marker_cursor = 0;
                            let (_, pane_rows) = active_pane_inner_size(
                                &windows[active_window].layout,
                                &windows[active_window].active_pane,
                                term_cols,
                                term_rows,
                            );
                            if markers.is_empty() {
                                status = Some("no markers in session".to_string());
                            } else {
                                status = jump_to_next_marker(
                                    &markers,
                                    &mut marker_cursor,
                                    &scrollback,
                                    usize::from(pane_rows),
                                    &mut scroll_offset,
                                    &mut scroll_mode,
                                );
                            }
                        } else if pending_worktree_list {
                            pending_worktree_list = false;
                            let sessions = parse_session_summaries(data)?;
                            status = Some(format_worktree_sessions(&metadata, &sessions));
                        }
                    }
                    Response::Error { message, .. } => bail!("server error: {message}"),
                    Response::Hello { .. } => {}
                }

                render_screen(
                    &mut out,
                    &args.session_id,
                    mode,
                    &metadata,
                    &windows[active_window],
                    active_window,
                    windows.len(),
                    &vt,
                    &scrollback,
                    scroll_mode,
                    scroll_offset,
                    search_regex.as_ref(),
                    search_pattern.as_deref(),
                    term_cols,
                    term_rows,
                    help_overlay,
                    status.as_deref(),
                )?;
            }
            input = input_rx.recv() => {
                match input {
                    Some(InputMsg::Quit) => done = true,
                    Some(InputMsg::PrefixArmed) => {
                        status = Some(
                            "prefix: c n/p 1-9 | - h/j/k/l d / m w ? [ esc"
                                .to_string(),
                        );
                    }
                    Some(InputMsg::SearchDraft(query)) => {
                        status = Some(format!("search /{query}"));
                    }
                    Some(InputMsg::SearchCommit(query)) => {
                        if query.trim().is_empty() {
                            search_pattern = None;
                            search_regex = None;
                            status = Some("search cleared".to_string());
                        } else {
                            match Regex::new(&query) {
                                Ok(regex) => {
                                    search_pattern = Some(query.clone());
                                    search_regex = Some(regex);
                                    scroll_mode = true;
                                    status = Some(format!("search active /{query}"));
                                }
                                Err(err) => {
                                    status = Some(format!("invalid regex: {err}"));
                                }
                            }
                        }
                    }
                    Some(InputMsg::Scroll(delta)) => {
                        let (_, pane_rows) = active_pane_inner_size(
                            &windows[active_window].layout,
                            &windows[active_window].active_pane,
                            term_cols,
                            term_rows,
                        );
                        apply_scroll_delta(
                            delta,
                            &mut scroll_offset,
                            &mut scroll_mode,
                            scrollback.line_count(),
                            usize::from(pane_rows),
                        );
                    }
                    Some(InputMsg::ExitScrollMode) => {
                        scroll_mode = false;
                        scroll_offset = 0;
                        status = Some("scroll mode off".to_string());
                    }
                    Some(InputMsg::PrefixCommand(cmd)) => {
                        let mut needs_resize = false;
                        match cmd {
                            PrefixCmd::NewWindow => {
                                windows.push(WindowState::new());
                                active_window = windows.len().saturating_sub(1);
                                status = Some(format!("window {}", active_window + 1));
                                needs_resize = true;
                            }
                            PrefixCmd::NextWindow => {
                                active_window = (active_window + 1) % windows.len();
                                status = Some(format!("window {}", active_window + 1));
                                needs_resize = true;
                            }
                            PrefixCmd::PrevWindow => {
                                if active_window == 0 {
                                    active_window = windows.len().saturating_sub(1);
                                } else {
                                    active_window -= 1;
                                }
                                status = Some(format!("window {}", active_window + 1));
                                needs_resize = true;
                            }
                            PrefixCmd::SelectWindow(index) => {
                                if index < windows.len() {
                                    active_window = index;
                                    status = Some(format!("window {}", active_window + 1));
                                    needs_resize = true;
                                } else {
                                    status = Some(format!("window {} not found", index + 1));
                                }
                            }
                            PrefixCmd::SplitHorizontal => {
                                let window = &mut windows[active_window];
                                if let Some(new_id) =
                                    window.layout.split(&window.active_pane, SplitAxis::Horizontal)
                                {
                                    window.active_pane = new_id;
                                    status = Some("split horizontal".to_string());
                                    needs_resize = true;
                                } else {
                                    status = Some("split failed".to_string());
                                }
                            }
                            PrefixCmd::SplitVertical => {
                                let window = &mut windows[active_window];
                                if let Some(new_id) =
                                    window.layout.split(&window.active_pane, SplitAxis::Vertical)
                                {
                                    window.active_pane = new_id;
                                    status = Some("split vertical".to_string());
                                    needs_resize = true;
                                } else {
                                    status = Some("split failed".to_string());
                                }
                            }
                            PrefixCmd::MoveFocus(direction) => {
                                let window = &mut windows[active_window];
                                if let Some(next) = adjacent_pane_id(
                                    &window.layout,
                                    &window.active_pane,
                                    term_cols,
                                    term_rows,
                                    direction,
                                ) {
                                    window.active_pane = next;
                                    status = Some(format!("active pane: {}", window.active_pane));
                                    needs_resize = true;
                                } else {
                                    status = Some("no adjacent pane".to_string());
                                }
                            }
                            PrefixCmd::GrowPane => {
                                let window = &mut windows[active_window];
                                if window.layout.resize_towards(&window.active_pane, 0.1) {
                                    status = Some("pane grown".to_string());
                                    needs_resize = true;
                                } else {
                                    status = Some("pane resize unavailable".to_string());
                                }
                            }
                            PrefixCmd::ShrinkPane => {
                                let window = &mut windows[active_window];
                                if window.layout.resize_towards(&window.active_pane, -0.1) {
                                    status = Some("pane shrunk".to_string());
                                    needs_resize = true;
                                } else {
                                    status = Some("pane resize unavailable".to_string());
                                }
                            }
                            PrefixCmd::Detach => {
                                done = true;
                            }
                            PrefixCmd::SearchPrompt => {
                                status = Some("search: type regex, Enter to apply".to_string());
                            }
                            PrefixCmd::MarkerJump => {
                                let (_, pane_rows) = active_pane_inner_size(
                                    &windows[active_window].layout,
                                    &windows[active_window].active_pane,
                                    term_cols,
                                    term_rows,
                                );
                                if markers.is_empty() {
                                    send_request(
                                        &mut writer,
                                        &Request::MarkerList {
                                            session_id: args.session_id.clone(),
                                        },
                                    )
                                    .await?;
                                    pending_marker_list = true;
                                    status = Some("loading markers...".to_string());
                                } else {
                                    status = jump_to_next_marker(
                                        &markers,
                                        &mut marker_cursor,
                                        &scrollback,
                                        usize::from(pane_rows),
                                        &mut scroll_offset,
                                        &mut scroll_mode,
                                    );
                                }
                            }
                            PrefixCmd::WorktreeSessions => {
                                send_request(&mut writer, &Request::SessionList).await?;
                                pending_worktree_list = true;
                                status = Some("loading worktree sessions...".to_string());
                            }
                            PrefixCmd::ToggleHelp => {
                                help_overlay = !help_overlay;
                                status = Some(if help_overlay {
                                    "help overlay on".to_string()
                                } else {
                                    "help overlay off".to_string()
                                });
                            }
                            PrefixCmd::EnterScrollMode => {
                                scroll_mode = true;
                                status = Some("scroll mode on".to_string());
                            }
                            PrefixCmd::Cancel => {
                                status = Some("prefix cancelled".to_string());
                            }
                        }

                        if needs_resize {
                            let (cols, rows) = active_pane_inner_size(
                                &windows[active_window].layout,
                                &windows[active_window].active_pane,
                                term_cols,
                                term_rows,
                            );
                            vt.resize(usize::from(cols), usize::from(rows));
                            if mode == AttachMode::Edit {
                                let _ = send_request(
                                    &mut writer,
                                    &Request::Resize {
                                        session_id: args.session_id.clone(),
                                        cols,
                                        rows,
                                    },
                                )
                                .await;
                            }
                        }
                    }
                    Some(InputMsg::Bytes(bytes)) => {
                        if mode == AttachMode::Edit && !scroll_mode {
                            send_request(
                                &mut writer,
                                &Request::SendInput {
                                    session_id: args.session_id.clone(),
                                    attachment_id: attachment_id.clone(),
                                    data_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
                                },
                            )
                            .await?;
                        }
                    }
                    Some(InputMsg::Resize(cols, rows)) => {
                        term_cols = cols.max(1);
                        term_rows = rows.max(4);
                        let (pty_cols, pty_rows) = active_pane_inner_size(
                            &windows[active_window].layout,
                            &windows[active_window].active_pane,
                            term_cols,
                            term_rows,
                        );
                        vt.resize(usize::from(pty_cols), usize::from(pty_rows));
                        if mode == AttachMode::Edit {
                            let _ = send_request(
                                &mut writer,
                                &Request::Resize {
                                    session_id: args.session_id.clone(),
                                    cols: pty_cols,
                                    rows: pty_rows,
                                },
                            )
                            .await;
                        }
                    }
                    None => done = true,
                }

                render_screen(
                    &mut out,
                    &args.session_id,
                    mode,
                    &metadata,
                    &windows[active_window],
                    active_window,
                    windows.len(),
                    &vt,
                    &scrollback,
                    scroll_mode,
                    scroll_offset,
                    search_regex.as_ref(),
                    search_pattern.as_deref(),
                    term_cols,
                    term_rows,
                    help_overlay,
                    status.as_deref(),
                )?;
            }
        }
    }

    let _ = send_request(
        &mut writer,
        &Request::Detach {
            attachment_id: attachment_id.clone(),
        },
    )
    .await;

    let _ = input_handle.join();
    Ok(())
}

fn active_pane_inner_size(
    layout: &PaneLayout,
    active_pane: &str,
    term_cols: u16,
    term_rows: u16,
) -> (u16, u16) {
    let pane_area_rows = term_rows.saturating_sub(1).max(1);
    let rects = layout.compute_rects(term_cols.max(1), pane_area_rows);
    let Some(rect) = rects.get(active_pane) else {
        return (term_cols.max(1), pane_area_rows.max(1));
    };
    let cols = rect.width.saturating_sub(2).max(1);
    let rows = rect.height.saturating_sub(2).max(1);
    (cols, rows)
}

fn adjacent_pane_id(
    layout: &PaneLayout,
    active_pane: &str,
    term_cols: u16,
    term_rows: u16,
    direction: Direction,
) -> Option<String> {
    let pane_area_rows = term_rows.saturating_sub(1).max(1);
    let rects = layout.compute_rects(term_cols.max(1), pane_area_rows);
    let active = rects.get(active_pane)?;

    let active_center_x = i32::from(active.x) + i32::from(active.width) / 2;
    let active_center_y = i32::from(active.y) + i32::from(active.height) / 2;

    let mut best: Option<(i32, i32, String)> = None;
    for (pane_id, rect) in rects {
        if pane_id == active_pane {
            continue;
        }

        let center_x = i32::from(rect.x) + i32::from(rect.width) / 2;
        let center_y = i32::from(rect.y) + i32::from(rect.height) / 2;

        let (primary, secondary) = match direction {
            Direction::Left if center_x < active_center_x => (
                active_center_x - center_x,
                (active_center_y - center_y).abs(),
            ),
            Direction::Right if center_x > active_center_x => (
                center_x - active_center_x,
                (active_center_y - center_y).abs(),
            ),
            Direction::Up if center_y < active_center_y => (
                active_center_y - center_y,
                (active_center_x - center_x).abs(),
            ),
            Direction::Down if center_y > active_center_y => (
                center_y - active_center_y,
                (active_center_x - center_x).abs(),
            ),
            _ => continue,
        };

        match &best {
            None => best = Some((primary, secondary, pane_id)),
            Some((best_primary, best_secondary, _)) => {
                let ord = (primary, secondary).cmp(&(*best_primary, *best_secondary));
                if ord == Ordering::Less {
                    best = Some((primary, secondary, pane_id));
                }
            }
        }
    }

    best.map(|(_, _, id)| id)
}

fn apply_scroll_delta(
    delta: i32,
    scroll_offset: &mut usize,
    scroll_mode: &mut bool,
    line_count: usize,
    viewport_rows: usize,
) {
    if delta == 0 || line_count == 0 {
        return;
    }

    let max_offset = line_count.saturating_sub(viewport_rows.max(1));
    if delta > 0 {
        *scroll_mode = true;
        let up = usize::try_from(delta).unwrap_or(0);
        *scroll_offset = scroll_offset.saturating_add(up).min(max_offset);
    } else {
        let down = usize::try_from(delta.saturating_abs()).unwrap_or(0);
        *scroll_offset = scroll_offset.saturating_sub(down);
        if *scroll_offset == 0 {
            *scroll_mode = false;
        }
    }
}

fn jump_to_next_marker(
    markers: &[MarkerEntry],
    marker_cursor: &mut usize,
    scrollback: &Scrollback,
    viewport_rows: usize,
    scroll_offset: &mut usize,
    scroll_mode: &mut bool,
) -> Option<String> {
    if markers.is_empty() {
        return Some("no markers".to_string());
    }

    let idx = *marker_cursor % markers.len();
    let marker = &markers[idx];
    *marker_cursor = (idx + 1) % markers.len();

    if let Some(line_idx) = scrollback.line_for_seq(marker.seq) {
        let line_count = scrollback.line_count();
        let target_top = line_idx.saturating_sub(viewport_rows / 2);
        let target_bottom = target_top.saturating_add(viewport_rows);
        *scroll_offset = line_count.saturating_sub(target_bottom);
        *scroll_mode = true;
        return Some(format!("marker {} @ seq {}", marker.name, marker.seq));
    }

    Some(format!(
        "marker {} (seq {}) not in local scrollback",
        marker.name, marker.seq
    ))
}

fn parse_markers(data: Option<Value>) -> Vec<MarkerEntry> {
    let Some(Value::Array(items)) = data else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in items {
        let Some(name) = item
            .get("name")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
        else {
            continue;
        };
        let Some(seq) = item.get("seq").and_then(|v| v.as_u64()) else {
            continue;
        };
        out.push(MarkerEntry { name, seq });
    }
    out
}

fn parse_session_summaries(data: Option<Value>) -> Result<Vec<SessionSummary>> {
    let Some(data) = data else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_value(data)?)
}

fn format_worktree_sessions(metadata: &PaneMetadata, sessions: &[SessionSummary]) -> String {
    let related: Vec<&SessionSummary> = sessions
        .iter()
        .filter(|s| s.git_repo_root == metadata.git_repo_root)
        .collect();

    if related.is_empty() {
        return "worktree sessions: none".to_string();
    }

    let names: Vec<String> = related
        .iter()
        .take(5)
        .map(|s| {
            if let Some(label) = &s.label {
                format!("{}:{}", s.session_id, label)
            } else {
                s.session_id.clone()
            }
        })
        .collect();

    format!(
        "worktree sessions: {} [{}]",
        related.len(),
        names.join(", ")
    )
}

#[allow(clippy::too_many_arguments)]
fn render_screen(
    out: &mut std::io::Stdout,
    session_id: &str,
    mode: AttachMode,
    metadata: &PaneMetadata,
    window: &WindowState,
    active_window: usize,
    window_count: usize,
    screen: &VtScreen,
    scrollback: &Scrollback,
    scroll_mode: bool,
    scroll_offset: usize,
    search_regex: Option<&Regex>,
    search_pattern: Option<&str>,
    term_cols: u16,
    term_rows: u16,
    help_overlay: bool,
    status: Option<&str>,
) -> Result<()> {
    let mode_tag = match mode {
        AttachMode::Edit => "[EDIT]",
        AttachMode::View => "[VIEW]",
    };
    let label = metadata.label.as_deref().unwrap_or("unlabeled");
    let branch = metadata
        .git_branch
        .as_ref()
        .map(|b| {
            if metadata.git_dirty == Some(true) {
                format!("{b}*")
            } else {
                b.clone()
            }
        })
        .unwrap_or_else(|| "-".to_string());
    let sandbox = if metadata.sandboxed {
        "sandbox:on"
    } else {
        "sandbox:off"
    };
    let scroll = if scroll_mode || scroll_offset > 0 {
        format!("scroll:{}", scroll_offset)
    } else {
        "scroll:tail".to_string()
    };
    let search = search_pattern
        .map(|p| format!("search:/{p}/"))
        .unwrap_or_else(|| "search:off".to_string());

    let mut header = format!(
        "tmax-client {mode_tag} | session={session_id} | label={label} | git={branch} | {sandbox} | window={}/{} | {scroll} | {search} | Ctrl-Q quit | Ctrl-Space prefix",
        active_window + 1,
        window_count,
    );
    if let Some(status) = status {
        header.push_str(" | ");
        header.push_str(status);
    }

    out.queue(Clear(ClearType::All))?
        .queue(MoveTo(0, 0))?
        .queue(Clear(ClearType::CurrentLine))?
        .queue(Print(truncate_string(&header, usize::from(term_cols))))?;

    let pane_area_rows = term_rows.saturating_sub(1).max(1);
    let rects = window
        .layout
        .compute_rects(term_cols.max(1), pane_area_rows);
    let mut ordered: Vec<(String, Rect)> = rects.into_iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));

    for (pane_id, rect) in &ordered {
        let abs = Rect {
            x: rect.x,
            y: rect.y.saturating_add(1),
            width: rect.width,
            height: rect.height,
        };
        let pane_title = if pane_id == &window.active_pane {
            format!("{} {mode_tag}", pane_id)
        } else {
            pane_id.to_string()
        };
        draw_pane_border(out, abs, &pane_title)?;

        if pane_id == &window.active_pane {
            let inner_rows = usize::from(abs.height.saturating_sub(2));
            let mut lines = if scroll_mode || scroll_offset > 0 {
                scrollback.window_lines(inner_rows, scroll_offset)
            } else {
                screen.lines()
            };

            if lines.len() > inner_rows {
                let start = lines.len().saturating_sub(inner_rows);
                lines = lines[start..].to_vec();
            }

            draw_active_pane_content(out, abs, &lines, search_regex)?;
        }
    }

    if help_overlay {
        draw_help_overlay(out, term_cols, term_rows)?;
    }

    out.flush()?;
    Ok(())
}

fn draw_pane_border(out: &mut std::io::Stdout, rect: Rect, title: &str) -> Result<()> {
    if rect.width < 2 || rect.height < 2 {
        return Ok(());
    }

    let inner_width = usize::from(rect.width.saturating_sub(2));
    let mut top = format!("+{}+", "-".repeat(inner_width));
    if inner_width > 0 {
        let label = truncate_string(title, inner_width);
        let start = 1usize;
        for (i, ch) in label.chars().enumerate() {
            if start + i < top.len().saturating_sub(1) {
                top.replace_range(start + i..start + i + 1, &ch.to_string());
            }
        }
    }

    let bottom = format!("+{}+", "-".repeat(inner_width));
    out.queue(MoveTo(rect.x, rect.y))?.queue(Print(top))?;
    for row in 1..rect.height.saturating_sub(1) {
        out.queue(MoveTo(rect.x, rect.y.saturating_add(row)))?
            .queue(Print("|"))?
            .queue(MoveTo(
                rect.x.saturating_add(rect.width.saturating_sub(1)),
                rect.y.saturating_add(row),
            ))?
            .queue(Print("|"))?;
    }
    out.queue(MoveTo(
        rect.x,
        rect.y.saturating_add(rect.height.saturating_sub(1)),
    ))?
    .queue(Print(bottom))?;
    Ok(())
}

fn draw_active_pane_content(
    out: &mut std::io::Stdout,
    rect: Rect,
    lines: &[String],
    search_regex: Option<&Regex>,
) -> Result<()> {
    if rect.width <= 2 || rect.height <= 2 {
        return Ok(());
    }

    let inner_width = usize::from(rect.width - 2);
    let inner_height = usize::from(rect.height - 2);

    for row in 0..inner_height {
        let y = rect
            .y
            .saturating_add(1)
            .saturating_add(u16::try_from(row).unwrap_or(0));
        let line = lines.get(row).cloned().unwrap_or_default();
        draw_line_with_optional_highlight(
            out,
            rect.x.saturating_add(1),
            y,
            &line,
            inner_width,
            search_regex,
        )?;
    }

    Ok(())
}

fn draw_line_with_optional_highlight(
    out: &mut std::io::Stdout,
    x: u16,
    y: u16,
    input: &str,
    width: usize,
    search_regex: Option<&Regex>,
) -> Result<()> {
    let line = truncate_string(input, width);
    out.queue(MoveTo(x, y))?;

    if let Some(regex) = search_regex {
        let mut cursor = 0usize;
        for mat in regex.find_iter(&line) {
            if mat.start() == mat.end() {
                continue;
            }

            if cursor < mat.start() {
                out.queue(Print(&line[cursor..mat.start()]))?;
            }

            out.queue(SetAttribute(Attribute::Reverse))?
                .queue(Print(&line[mat.start()..mat.end()]))?
                .queue(SetAttribute(Attribute::Reset))?;
            cursor = mat.end();
        }

        if cursor < line.len() {
            out.queue(Print(&line[cursor..]))?;
        }

        let printed = line.chars().count();
        if printed < width {
            out.queue(Print(" ".repeat(width - printed)))?;
        }
        return Ok(());
    }

    out.queue(Print(pad_or_truncate(&line, width)))?;
    Ok(())
}

fn draw_help_overlay(out: &mut std::io::Stdout, term_cols: u16, term_rows: u16) -> Result<()> {
    let lines = [
        "tmax-client help",
        "c new window | n/p next/prev window | 1-9 switch window",
        "| vertical split | - horizontal split | h/j/k/l move focus",
        "d detach | / search regex | m marker jump | w worktree sessions",
        "[ enter scroll mode | mouse wheel smooth scroll | Esc exit scroll",
        "? toggle this help | Ctrl-Q quit",
    ];

    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(10)
        .saturating_add(4)
        .min(usize::from(term_cols));
    let height = lines.len().saturating_add(2).min(usize::from(term_rows));

    let x = usize::from(term_cols)
        .saturating_sub(width)
        .checked_div(2)
        .unwrap_or(0);
    let y = usize::from(term_rows)
        .saturating_sub(height)
        .checked_div(2)
        .unwrap_or(0);

    for row in 0..height {
        let abs_y = u16::try_from(y.saturating_add(row)).unwrap_or(0);
        let border_line = if row == 0 || row == height.saturating_sub(1) {
            format!("+{}+", "-".repeat(width.saturating_sub(2)))
        } else {
            let content_idx = row.saturating_sub(1);
            let content = lines.get(content_idx).copied().unwrap_or("");
            let inner_w = width.saturating_sub(2);
            let content = pad_or_truncate(content, inner_w);
            format!("|{content}|")
        };

        out.queue(MoveTo(u16::try_from(x).unwrap_or(0), abs_y))?
            .queue(Print(truncate_string(&border_line, usize::from(term_cols))))?;
    }

    Ok(())
}

fn truncate_string(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

fn pad_or_truncate(input: &str, width: usize) -> String {
    let truncated: String = input.chars().take(width).collect();
    if truncated.chars().count() >= width {
        return truncated;
    }
    let mut out = truncated;
    out.push_str(&" ".repeat(width.saturating_sub(out.chars().count())));
    out
}

fn spawn_input_thread(tx: mpsc::UnboundedSender<InputMsg>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut prefix_mode = false;
        let mut search_mode: Option<String> = None;

        while let Ok(event) = event::read() {
            match event {
                CtEvent::Resize(cols, rows) => {
                    let _ = tx.send(InputMsg::Resize(cols, rows));
                }
                CtEvent::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        let _ = tx.send(InputMsg::Scroll(3));
                    }
                    MouseEventKind::ScrollDown => {
                        let _ = tx.send(InputMsg::Scroll(-3));
                    }
                    _ => {}
                },
                CtEvent::Key(key) => {
                    if is_quit_key(key) {
                        let _ = tx.send(InputMsg::Quit);
                        break;
                    }

                    if let Some(query) = &mut search_mode {
                        match key.code {
                            KeyCode::Enter => {
                                let _ = tx.send(InputMsg::SearchCommit(query.clone()));
                                search_mode = None;
                            }
                            KeyCode::Esc => {
                                let _ = tx.send(InputMsg::SearchCommit(String::new()));
                                search_mode = None;
                            }
                            KeyCode::Backspace => {
                                query.pop();
                                let _ = tx.send(InputMsg::SearchDraft(query.clone()));
                            }
                            KeyCode::Char(c) => {
                                query.push(c);
                                let _ = tx.send(InputMsg::SearchDraft(query.clone()));
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if prefix_mode {
                        prefix_mode = false;
                        let cmd = match key.code {
                            KeyCode::Char('c') => PrefixCmd::NewWindow,
                            KeyCode::Char('n') => PrefixCmd::NextWindow,
                            KeyCode::Char('p') => PrefixCmd::PrevWindow,
                            KeyCode::Char('|') | KeyCode::Char('v') => PrefixCmd::SplitVertical,
                            KeyCode::Char('-') | KeyCode::Char('s') => PrefixCmd::SplitHorizontal,
                            KeyCode::Char('h') => PrefixCmd::MoveFocus(Direction::Left),
                            KeyCode::Char('j') => PrefixCmd::MoveFocus(Direction::Down),
                            KeyCode::Char('k') => PrefixCmd::MoveFocus(Direction::Up),
                            KeyCode::Char('l') => PrefixCmd::MoveFocus(Direction::Right),
                            KeyCode::Char('+') | KeyCode::Char('=') => PrefixCmd::GrowPane,
                            KeyCode::Char('_') => PrefixCmd::ShrinkPane,
                            KeyCode::Char('d') => PrefixCmd::Detach,
                            KeyCode::Char('m') => PrefixCmd::MarkerJump,
                            KeyCode::Char('w') => PrefixCmd::WorktreeSessions,
                            KeyCode::Char('?') => PrefixCmd::ToggleHelp,
                            KeyCode::Char('[') => PrefixCmd::EnterScrollMode,
                            KeyCode::Char('/') => {
                                search_mode = Some(String::new());
                                let _ = tx.send(InputMsg::PrefixCommand(PrefixCmd::SearchPrompt));
                                continue;
                            }
                            KeyCode::Char('1') => PrefixCmd::SelectWindow(0),
                            KeyCode::Char('2') => PrefixCmd::SelectWindow(1),
                            KeyCode::Char('3') => PrefixCmd::SelectWindow(2),
                            KeyCode::Char('4') => PrefixCmd::SelectWindow(3),
                            KeyCode::Char('5') => PrefixCmd::SelectWindow(4),
                            KeyCode::Char('6') => PrefixCmd::SelectWindow(5),
                            KeyCode::Char('7') => PrefixCmd::SelectWindow(6),
                            KeyCode::Char('8') => PrefixCmd::SelectWindow(7),
                            KeyCode::Char('9') => PrefixCmd::SelectWindow(8),
                            KeyCode::Esc => PrefixCmd::Cancel,
                            _ => PrefixCmd::Cancel,
                        };
                        let _ = tx.send(InputMsg::PrefixCommand(cmd));
                        continue;
                    }

                    if is_prefix_key(key) {
                        prefix_mode = true;
                        let _ = tx.send(InputMsg::PrefixArmed);
                        continue;
                    }

                    if key.code == KeyCode::Esc {
                        let _ = tx.send(InputMsg::ExitScrollMode);
                        continue;
                    }

                    if let Some(bytes) = key_to_bytes(key) {
                        let _ = tx.send(InputMsg::Bytes(bytes));
                    }
                }
                _ => {}
            }
        }
    })
}

fn is_prefix_key(key: KeyEvent) -> bool {
    (key.code == KeyCode::Char(' ') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn is_quit_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn key_to_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    match key.code {
        KeyCode::Enter => Some(vec![b'\n']),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Char(c) => Some(c.to_string().into_bytes()),
        _ => None,
    }
}

async fn read_hello(
    reader: &mut FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
) -> Result<()> {
    loop {
        let Some(line) = reader.next().await else {
            bail!("server disconnected before hello");
        };
        let line = line?;
        let resp: Response = serde_json::from_str(&line)?;
        if let Response::Hello { .. } = resp {
            return Ok(());
        }
    }
}

async fn request_session_summary(
    writer: &mut FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
    reader: &mut FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
    session_id: &str,
) -> Result<SessionSummary> {
    send_request(
        writer,
        &Request::SessionInfo {
            session_id: session_id.to_string(),
        },
    )
    .await?;
    loop {
        let Some(line) = reader.next().await else {
            bail!("server disconnected before session info");
        };
        let line = line?;
        let resp: Response = serde_json::from_str(&line)?;
        match resp {
            Response::Ok { data } => {
                let data = data.ok_or_else(|| anyhow!("missing session info payload"))?;
                let summary: SessionSummary = serde_json::from_value(data)?;
                return Ok(summary);
            }
            Response::Error { message, .. } => bail!("session info failed: {message}"),
            Response::Hello { .. } | Response::Event { .. } => {}
        }
    }
}

async fn wait_for_attachment(
    reader: &mut FramedRead<tokio::net::unix::OwnedReadHalf, LinesCodec>,
) -> Result<String> {
    loop {
        let Some(line) = reader.next().await else {
            bail!("server disconnected before attach ack");
        };
        let line = line?;
        let resp: Response = serde_json::from_str(&line)?;
        match resp {
            Response::Ok { data } => {
                if let Some(data) = data
                    && let Some(id) = data
                        .get("attachment")
                        .and_then(|a| a.get("attachment_id"))
                        .and_then(|v| v.as_str())
                {
                    return Ok(id.to_string());
                }
            }
            Response::Error { message, .. } => bail!("attach failed: {message}"),
            Response::Hello { .. } | Response::Event { .. } => {}
        }
    }
}

async fn send_request(
    writer: &mut FramedWrite<tokio::net::unix::OwnedWriteHalf, LinesCodec>,
    req: &Request,
) -> Result<()> {
    let line = serde_json::to_string(req)?;
    writer.send(line).await?;
    Ok(())
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut socket_path = default_socket_path();
        let mut view = false;
        let mut headless_smoke = false;
        let mut session_id = None;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--socket" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow!("--socket requires a value"))?;
                    socket_path = PathBuf::from(value);
                }
                "--view" => view = true,
                "--headless-smoke" => headless_smoke = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                value if value.starts_with('-') => bail!("unknown flag: {value}"),
                value => {
                    if session_id.is_none() {
                        session_id = Some(value.to_string());
                    } else {
                        bail!("unexpected argument: {value}");
                    }
                }
            }
        }

        let session_id = session_id.ok_or_else(|| anyhow!("missing required <session_id>"))?;
        Ok(Self {
            session_id,
            socket_path,
            view,
            headless_smoke,
        })
    }
}

fn print_help() {
    println!("tmax-client [--socket PATH] [--view] [--headless-smoke] <session_id>");
}

fn default_socket_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("tmax").join("tmax.sock");
    }

    let uid = Uid::effective().as_raw();
    PathBuf::from(format!("/tmp/tmax-{uid}/tmax.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_translation_basic() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(key_to_bytes(enter), Some(vec![b'\n']));
    }

    #[test]
    fn prefix_key_detects_ctrl_space() {
        let key = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert!(is_prefix_key(key));
    }

    #[test]
    fn scrollback_seq_lookup_tracks_recent_line() {
        let mut sb = Scrollback::new(32);
        sb.ingest_output(1, b"one\n");
        sb.ingest_output(2, b"two\n");
        assert_eq!(sb.line_for_seq(1), Some(0));
        assert_eq!(sb.line_for_seq(2), Some(1));
    }

    #[test]
    fn parse_markers_reads_name_and_seq() {
        let markers = parse_markers(Some(serde_json::json!([
            {"name": "start", "seq": 7, "timestamp": "ignored"},
            {"name": "mid", "seq": 11}
        ])));
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0].name, "start");
        assert_eq!(markers[1].seq, 11);
    }
}
