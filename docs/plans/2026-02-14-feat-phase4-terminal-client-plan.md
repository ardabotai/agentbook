---
title: "Phase 4: Terminal Client (tmax-client)"
type: feat
status: active
date: 2026-02-14
---

# Phase 4: Terminal Client (tmax-client)

## Overview

Build a native terminal UI (`tmax-client` crate) that connects to `tmax-server` over the existing Unix socket protocol and provides a full terminal multiplexer experience: pane splits, scrollback, search, markers, keybindings, and mouse support. The client renders virtual terminal output using `vt100` for screen buffer management and `crossterm` for terminal I/O.

## Problem Statement

The current `tmax attach` command (in `tmax-cli`) streams raw PTY bytes to stdout but has no:
- Input forwarding (stdin to PTY)
- Detach keybinding
- Terminal resize handling
- Multi-pane layout
- Scrollback/search UX
- Pane borders with metadata

Users need a proper terminal multiplexer UI to interact with tmax sessions, competitive with tmux/zellij for basic workflows.

## Architecture Decisions

### 1. Separate crate, not an extension of tmax-cli

The client is a new `tmax-client` crate producing a `tmax-attach` binary. This keeps the CLI tool (`tmax`) focused on scripting/automation and the client focused on interactive terminal UI. The client depends on `tmax-protocol` only (not `libtmax`), same pattern as `tmax-cli`.

### 2. Use `vt100` crate, not raw `vte`

The `vt100` crate (v0.16) wraps `vte` and provides a complete virtual terminal screen buffer with:
- `Parser::new(rows, cols, scrollback)` - create parser with screen buffer
- `parser.process(bytes)` - feed raw PTY output
- `parser.screen().cell(row, col)` - read cell content and attributes
- `parser.screen().contents_diff(&prev_screen)` - efficient differential rendering

This eliminates the need to build a custom screen buffer + ANSI state machine. The `vt100` crate handles all escape sequences, cursor movement, scrolling regions, and alternate screen mode.

### 3. Tree-based pane layout

Pane layout uses a binary tree structure inspired by Warp/Zellij:
```
enum LayoutNode {
    Pane { id: PaneId, ... },
    Split { direction: Direction, ratio: f32, children: [Box<LayoutNode>; 2] },
}
```
- Splits are recursive: splitting a pane replaces it with a Split node containing the original + a new pane
- Resize adjusts the `ratio` of the nearest Split ancestor
- Navigation (h/j/k/l) finds the nearest pane in the given direction by walking the tree
- O(N) add/remove where N = number of panes

### 4. Async event loop with `tokio::select!`

The main loop concurrently handles:
- Terminal input events (crossterm `EventStream`)
- Server messages (socket read)
- Terminal resize signals (crossterm resize events)

```rust
loop {
    tokio::select! {
        Some(Ok(event)) = input_stream.next() => handle_input(event),
        Some(msg) = client.read_line() => handle_server_message(msg),
    }
}
```

### 5. Client-side VT parsing

The server streams raw PTY bytes. The client maintains per-pane `vt100::Parser` instances. This means:
- No server-side rendering overhead
- Each client can have different terminal sizes
- Scrollback buffer is client-side

## Design Decisions & Clarifications

### Panes and windows are client-side only

Panes and windows are purely client-local layout concepts. The server knows about sessions; the client arranges sessions into a visual grid. Two clients viewing the same server will have independent layouts. No protocol changes needed for pane/window management.

### Launch behavior

- Binary: `tmax-attach <session-id>` attaches to a specific session (single-pane)
- `tmax-attach` with no args: lists sessions and prompts to select one (future enhancement, not in v1)
- If server is not running: print error "tmax server is not running. Start it with: tmax server start" and exit (same as CLI)

### Prefix key behavior

- `Ctrl+Space` enters prefix mode with a **2-second timeout**
- Visual indicator: status bar shows `[PREFIX]` in yellow while prefix mode is active
- Recognized key: execute action, return to normal mode
- Unrecognized key: forward both `Ctrl+Space` and the key to PTY, return to normal mode
- To send literal `Ctrl+Space`: press `Ctrl+Space, Ctrl+Space` (double-tap sends one to PTY)
- Timeout: return to normal mode, forward nothing

### View mode behavior

- View mode is **not** inherently scroll mode - it's normal rendering with input disabled
- In view mode, `Ctrl+Space` prefix still works but only for: `/` (search), `m` (markers), `[` (scroll mode), `d` (detach), `?` (help)
- All other prefix keys are ignored (no split, no window management)
- Non-prefix keys are silently dropped (no forwarding to PTY)
- Status bar shows `[VIEW]` indicator

### New split session creation

- Splitting creates a new server session with `exec: $SHELL`, `cwd` inherited from the source pane's session, no sandbox
- The new session is auto-attached in edit mode
- Each pane can show a different session

### Session exit behavior

- When a session exits, the pane shows `[exited: code N]` in the status bar and freezes output
- The user can close the pane with `Ctrl+Space, x` or it auto-closes after 5 seconds
- If the last pane exits, the client exits

### Multi-client viewport independence

- Scrolling is entirely client-side. Each client has its own `vt100::Parser` and scrollback buffer. One client scrolling does not affect other clients viewing the same session.
- **PTY size is controlled by the edit client only.** The server-side PTY has one size; all subscribers receive the same raw bytes generated for that size.
- View clients parse the bytes through their own `vt100::Parser` but the content was generated for the edit client's dimensions:
  - Viewer with a **larger** terminal: content renders in top-left with empty space
  - Viewer with a **smaller** terminal: content is cropped (viewport panning deferred to post-v1)
- Search state is also per-client and independent.

### Scrollback model

- Client-side only via `vt100::Parser` with 10,000 lines scrollback
- No server-side scrollback requests needed (LiveBuffer replay on subscribe provides catch-up)
- If LiveBuffer has wrapped, the client only sees output from subscribe point forward (acceptable for v1)

### Copy/paste

- Deferred to post-v1. Terminal's native selection (shift+click) still works since we don't capture shift+mouse
- OSC 52 clipboard sequences pass through to the host terminal

### Error handling

- Server disconnect: show `[server disconnected]` in status bar, restore terminal, exit after 2s
- Edit-attach rejection (another client has edit): fall back to view mode, show warning in status bar
- No automatic reconnection in v1

### Rendering strategy

- Event-driven: render on new output, not on a timer
- Per-pane damage tracking via `vt100::Screen::contents_diff()`
- Batch crossterm commands with `queue!()` macro, flush once per render cycle

### Minimum terminal size

- Minimum: 40 columns x 10 rows
- Below minimum: show error message "terminal too small" and wait for resize
- Minimum pane size for splitting: 20 cols x 5 rows (content area)

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `tmax-protocol` | workspace | Server communication types |
| `crossterm` | 0.29 | Terminal I/O, raw mode, events, mouse |
| `vt100` | 0.16 | Virtual terminal screen buffer + ANSI parsing |
| `tokio` | workspace | Async runtime |
| `unicode-width` | 0.1 | CJK/wide character width calculation |
| `regex` | 1 | Scrollback search |
| `serde` | workspace | Serialization |
| `serde_json` | workspace | JSON protocol |
| `anyhow` | 1 | Error handling |
| `tracing` | workspace | Logging |

## Implementation Phases

### Phase 4.1: Single-Pane Attach with Rendering

**Goal:** Replace the broken `tmax attach` with a working single-pane terminal client that correctly renders output, forwards input, handles resize, and supports detach.

**Tasks:**
- [x] Create `crates/tmax-client/` crate with `tmax-attach` binary
- [x] Add `tmax-client` to workspace `Cargo.toml`
- [x] Implement `ServerConnection` (adapted from `TmaxClient` in `crates/tmax-cli/src/client.rs:6`) with split read/write for concurrent use in `tokio::select!`
- [x] Implement `TerminalState` struct wrapping `vt100::Parser` for a single session
- [x] Implement main event loop: crossterm `EventStream` + server socket read via `tokio::select!`
- [x] Implement input forwarding: terminal keystrokes -> `Request::SendInput` to server
- [x] Implement output rendering: server `Event::Output` bytes -> `vt100::Parser::process()` -> differential render to real terminal via crossterm
- [x] Implement terminal resize: crossterm `Event::Resize` -> `Request::Resize` to server + recreate vt100 parser with new dimensions
- [x] Enter alternate screen + raw mode on attach, restore on detach/exit
- [x] Implement `Ctrl+Space, d` to detach (send `Request::Detach`, restore terminal, exit)
- [x] Handle `Event::SessionExited` gracefully (restore terminal, print exit code, exit)
- [x] Add basic status bar at bottom row showing: session ID, label, `[EDIT]`/`[VIEW]`, git branch
- [ ] Add integration test: connect to server, send input, verify output round-trip

**Acceptance Criteria:**
- `tmax-attach <session-id>` connects and renders output correctly (colors, cursor positioning, alternate screen apps like vim work)
- Typing forwards keystrokes to the PTY
- Terminal resize propagates to PTY
- `Ctrl+Space, d` detaches cleanly
- Session exit restores the terminal properly

### Phase 4.2: Multi-Pane Layout Engine

**Goal:** Add pane splitting, navigation, borders, and metadata display.

**Tasks:**
- [ ] Implement `LayoutTree` with `LayoutNode` enum (Pane vs Split)
- [ ] Implement `Pane` struct: `PaneId`, `session_id`, `vt100::Parser`, `attachment_mode`, bounds `(x, y, w, h)`
- [ ] Implement split operations: `Ctrl+Space, |` (vertical) and `Ctrl+Space, -` (horizontal)
  - Splitting creates a new session via `Request::SessionCreate` (using user's `$SHELL`)
  - Replaces the current pane node with a Split node
  - Subscribes to the new session's event stream
- [ ] Implement `layout_tree.compute_bounds(total_cols, total_rows)` - recursive bounds calculation accounting for 1-char border lines
- [ ] Implement pane border rendering with box-drawing characters (`│`, `─`, `┌`, `┐`, `└`, `┘`, `├`, `┤`, `┬`, `┴`, `┼`)
- [ ] Implement pane metadata in borders: `[label] [git-branch] [EDIT/VIEW]`
- [ ] Implement pane navigation: `Ctrl+Space, h/j/k/l` (vim-style directional focus)
- [ ] Implement pane close: `Ctrl+Space, x` - kill session + remove pane from tree, sibling takes full space
- [ ] Implement pane resize: `Ctrl+Space, H/J/K/L` (shift+direction to grow focused pane)
- [ ] Route input to focused pane only
- [ ] Route each session's output to its corresponding pane's `vt100::Parser`
- [ ] Manage multiple concurrent subscriptions (one per pane/session)
- [ ] Full redraw on terminal resize: recompute all pane bounds, resize all sessions, re-render all panes

**Acceptance Criteria:**
- Can split panes horizontally and vertically, creating nested splits
- Pane borders display correctly with metadata
- Can navigate between panes with h/j/k/l
- Input goes to focused pane only
- Closing a pane removes it and the sibling expands
- Terminal resize correctly reflows all panes

### Phase 4.3: Windows and Session Management

**Goal:** Add window (tab) support and session management keybindings.

**Tasks:**
- [ ] Implement `Window` struct containing a `LayoutTree` and a label
- [ ] Implement window list in status bar: `[1:shell] [2:feat-auth] *3:debug`
- [ ] Implement `Ctrl+Space, c` - create new window (new session + new layout tree)
- [ ] Implement `Ctrl+Space, n/p` - next/previous window
- [ ] Implement `Ctrl+Space, 1-9` - switch to window N
- [ ] Implement `Ctrl+Space, w` - list worktree sessions (show session tree, select to attach)
- [ ] Implement `Ctrl+Space, m` - list markers for current session (select to jump)
- [ ] Implement `Ctrl+Space, ?` - help overlay showing all keybindings
- [ ] Implement view mode: `tmax-attach --view <session-id>` disables input forwarding and session control keybindings

**Acceptance Criteria:**
- Can create and switch between windows
- Status bar shows window list with active indicator
- Window management keybindings work
- Help overlay displays correctly
- View mode prevents input/control but allows scroll/search

### Phase 4.4: Scrollback and Search

**Goal:** Add scroll mode, search with regex highlighting, and marker navigation.

**Tasks:**
- [ ] Implement scrollback buffer: store historical screen states or raw output bytes per pane
  - Option A: Use `vt100::Parser` with large scrollback parameter
  - Option B: Store raw bytes + re-parse on scroll (more memory-efficient for large buffers)
  - **Decision: Use Option A** (`vt100::Parser::new(rows, cols, 10_000)` for 10k lines scrollback)
- [ ] Implement scroll mode entry: `Ctrl+Space, [` or `PageUp` when at bottom
- [ ] Implement scroll navigation: `j/k` (line), `Ctrl+d/u` (half-page), `g/G` (top/bottom)
- [ ] Implement scroll mode exit: `q` or `Escape` or any input in edit mode
- [ ] Implement visual indicator when in scroll mode: `[SCROLL +N lines]` in status bar
- [ ] Implement search: `Ctrl+Space, /` enters search mode, type regex, highlight matches
- [ ] Implement search navigation: `n/N` for next/previous match
- [ ] Implement marker jump: `Ctrl+Space, m` shows marker list, selecting jumps to that output position
- [ ] Implement mouse wheel scrolling: scroll up enters scroll mode, scroll down at bottom exits

**Acceptance Criteria:**
- Can scroll through output history
- Search highlights regex matches and navigates between them
- Markers are jumpable from the marker list
- Mouse wheel scrolling works
- Scroll mode clearly indicated in UI

### Phase 4.5: Mouse Support and Polish

**Goal:** Full mouse support and UI polish for a production-ready experience.

**Tasks:**
- [ ] Enable mouse capture via `crossterm::event::EnableMouseCapture`
- [ ] Implement click-to-focus: clicking a pane makes it the focused pane
- [ ] Implement drag-to-resize: dragging pane borders adjusts split ratios
- [ ] Implement mouse wheel scroll (already partially done in 4.4)
- [ ] Implement true color support: map `vt100` cell colors to crossterm colors (16, 256, and RGB)
- [ ] Implement Unicode/wide character support: use `unicode-width` for correct column alignment of CJK characters
- [ ] Handle edge cases: very small terminal sizes (minimum viable: 40x10), single-pane-only when too small to split
- [ ] Performance optimization: only redraw changed panes (diff-based rendering via `vt100::Screen::contents_diff`)
- [ ] Add `--cols` and `--rows` override flags for testing
- [ ] Comprehensive integration tests for the full client

**Acceptance Criteria:**
- Mouse clicks focus panes
- Mouse drag resizes splits
- True color output renders correctly
- Wide characters display properly
- Small terminals degrade gracefully
- Rendering is efficient (no full-screen redraws on every output byte)

## File Structure

```
crates/tmax-client/
├── Cargo.toml
├── src/
│   ├── main.rs           # CLI args (clap), connect, launch event loop
│   ├── connection.rs     # ServerConnection (split async read/write)
│   ├── terminal.rs       # Terminal setup/teardown (raw mode, alternate screen, mouse)
│   ├── event_loop.rs     # Main tokio::select! loop
│   ├── pane.rs           # Pane struct (vt100::Parser, session_id, bounds)
│   ├── layout.rs         # LayoutNode tree, compute_bounds, split/remove/navigate
│   ├── window.rs         # Window struct (layout tree + label), window list
│   ├── renderer.rs       # Differential rendering: vt100 screen -> crossterm output
│   ├── keybindings.rs    # Prefix key state machine, action dispatch
│   ├── scroll.rs         # Scroll mode state, search, marker jump
│   ├── status_bar.rs     # Bottom status bar rendering
│   └── mouse.rs          # Mouse event handling (click focus, drag resize)
```

## Keybinding Reference

All keybindings use `Ctrl+Space` as the prefix key.

| Keybinding | Action | Phase |
|-----------|--------|-------|
| `Ctrl+Space, d` | Detach | 4.1 |
| `Ctrl+Space, \|` | Vertical split | 4.2 |
| `Ctrl+Space, -` | Horizontal split | 4.2 |
| `Ctrl+Space, h/j/k/l` | Navigate panes | 4.2 |
| `Ctrl+Space, H/J/K/L` | Resize pane | 4.2 |
| `Ctrl+Space, x` | Close pane | 4.2 |
| `Ctrl+Space, c` | New window | 4.3 |
| `Ctrl+Space, n/p` | Next/prev window | 4.3 |
| `Ctrl+Space, 1-9` | Switch to window N | 4.3 |
| `Ctrl+Space, w` | List worktree sessions | 4.3 |
| `Ctrl+Space, m` | List markers | 4.3 |
| `Ctrl+Space, ?` | Help overlay | 4.3 |
| `Ctrl+Space, [` | Enter scroll mode | 4.4 |
| `Ctrl+Space, /` | Search scrollback | 4.4 |

**In scroll mode** (no prefix needed):
| Key | Action |
|-----|--------|
| `j/k` | Scroll down/up one line |
| `Ctrl+d/u` | Half-page down/up |
| `g/G` | Top/bottom |
| `n/N` | Next/prev search match |
| `q` or `Escape` | Exit scroll mode |

## Technical Considerations

### Rendering Strategy

Each pane maintains its own `vt100::Parser`. On output:
1. Feed raw bytes to `parser.process(bytes)`
2. Call `parser.screen().contents_diff(&prev_screen)` to get only changed cells
3. For each changed cell, emit crossterm commands: `MoveTo(x + pane.x, y + pane.y)`, `SetColors(...)`, `Print(char)`
4. Save current screen as `prev_screen` for next diff

This is significantly more efficient than full-screen redraws.

### Concurrent Subscriptions

Each pane subscribes to its session's event stream. The event loop must handle messages from multiple sessions simultaneously. The `ServerConnection` needs to demux incoming events by `session_id` and route them to the correct pane.

One design question: **one connection per pane or one multiplexed connection?**
- The current protocol supports subscribing to multiple sessions on one connection
- Use **one connection, multiple subscriptions** - simpler, fewer file descriptors
- Route events by `session_id` field in the Event enum

### Prefix Key State Machine

```
State::Normal -> Ctrl+Space -> State::Prefix
State::Prefix -> recognized key -> execute action, -> State::Normal
State::Prefix -> unrecognized key -> forward keystroke, -> State::Normal
State::Prefix -> timeout (2s) -> -> State::Normal
```

### Terminal Size Constraints

Minimum terminal size for multi-pane: each pane needs at least 10 cols x 3 rows (content) + 1 for border. If terminal is too small to accommodate a split, reject the split with a status bar message.

## Risk Analysis

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| vt100 crate doesn't handle some escape sequences | Low | Medium | Fall back to raw vte if needed; vt100 is well-maintained and used by alacritty |
| Performance with many panes | Medium | Medium | Diff-based rendering limits redraw scope; profile early |
| Complex pane border rendering with nested splits | Medium | Low | Start simple (single-char borders), polish later |
| Mouse drag resize feels janky | Medium | Low | Debounce resize events; can ship without drag initially |

## Success Metrics

- `tmax-attach <session>` renders output identically to directly running the command in a terminal
- Can split into 4+ panes and navigate between them fluidly
- Scrollback search finds matches in < 100ms for 10k lines
- Full terminal resize completes in < 50ms
- No visual artifacts during rapid output (e.g., `yes | head -1000`)

## References

- [crossterm 0.29 docs](https://docs.rs/crossterm/0.29)
- [vt100 0.16 docs](https://docs.rs/vt100/0.16)
- [unicode-width docs](https://docs.rs/unicode-width)
- Existing client code: `crates/tmax-cli/src/client.rs`, `crates/tmax-cli/src/commands.rs:261` (attach)
- Brainstorm: `docs/brainstorms/2026-02-14-tmax-terminal-multiplexer-brainstorm.md` (lines 153-208)
- Parent plan: `docs/plans/2026-02-14-feat-tmax-terminal-multiplexer-plan.md` (lines 591-618)
