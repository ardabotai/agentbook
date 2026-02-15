---
title: "Phase 4: Terminal Client (tmax-client)"
type: feat
status: completed
date: 2026-02-14
---

# Phase 4: Terminal Client (tmax-client)

## Overview

Build a native terminal UI (`tmax-client` crate) that connects to `tmax-server` over the existing Unix socket protocol and provides a single-session terminal experience: scrollback, search, markers, keybindings, and mouse support. The client renders virtual terminal output using `vt100` for screen buffer management and `crossterm` for terminal I/O. Multi-pane layout is intentionally out of scope — tmax is a programmable API-first tool, and pane management belongs in the web GUI, not a terminal emulator.

## Problem Statement

The current `tmax attach` command (in `tmax-cli`) streams raw PTY bytes to stdout but has no:
- Input forwarding (stdin to PTY)
- Detach keybinding
- Terminal resize handling
- Scrollback/search UX

Users need a proper terminal client to interact with individual tmax sessions.

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

### 3. Async event loop with `tokio::select!`

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

### 4. Client-side VT parsing

The server streams raw PTY bytes. The client maintains a `vt100::Parser` instance. This means:
- No server-side rendering overhead
- Each client can have different terminal sizes
- Scrollback buffer is client-side

## Design Decisions & Clarifications

### Single-session client

The terminal client attaches to one session at a time. Multi-session viewing is handled by the web GUI (tmax-web). This keeps the client simple and focused — it's a lightweight attach tool, not a terminal multiplexer.

### Launch behavior

- Binary: `tmax-attach <session-id>` attaches to a specific session
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

- View mode renders output normally but input is disabled
- In view mode, `Ctrl+Space` prefix still works but only for: `d` (detach), `?` (help)
- All other prefix keys are ignored
- Non-prefix keys are silently dropped (no forwarding to PTY)
- Status bar shows `[VIEW]` indicator

### Session exit behavior

- When a session exits, the client shows `[exited: code N]` in the status bar and freezes output
- The client exits after 5 seconds or on any keypress

### Multi-client viewport independence

- Scrolling is entirely client-side. Each client has its own `vt100::Parser` and scrollback buffer. One client scrolling does not affect other clients viewing the same session.
- **PTY size is controlled by the edit client only.** The server-side PTY has one size; all subscribers receive the same raw bytes generated for that size.
- View clients parse the bytes through their own `vt100::Parser` but the content was generated for the edit client's dimensions:
  - Viewer with a **larger** terminal: content renders in top-left with empty space
  - Viewer with a **smaller** terminal: content is cropped (viewport panning deferred to post-v1)
- Search state is also per-client and independent.

### Copy/paste

- Terminal's native selection (shift+click) works since we don't capture mouse
- OSC 52 clipboard sequences pass through to the host terminal

### Error handling

- Server disconnect: show `[server disconnected]` in status bar, restore terminal, exit after 2s
- Edit-attach rejection (another client has edit): fall back to view mode, show warning in status bar
- No automatic reconnection in v1

### Rendering strategy

- Event-driven: render on new output, not on a timer
- Damage tracking via `vt100::Screen::contents_diff()`
- Batch crossterm commands with `queue!()` macro, flush once per render cycle

### Minimum terminal size

- Minimum: 40 columns x 10 rows
- Below minimum: show error message "terminal too small" and wait for resize


## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `tmax-protocol` | workspace | Server communication types |
| `crossterm` | 0.29 | Terminal I/O, raw mode, events, mouse |
| `vt100` | 0.16 | Virtual terminal screen buffer + ANSI parsing |
| `tokio` | workspace | Async runtime |
| `unicode-width` | 0.1 | CJK/wide character width calculation |
| `serde` | workspace | Serialization |
| `serde_json` | workspace | JSON protocol |
| `anyhow` | 1 | Error handling |
| `tracing` | workspace | Logging |

## Implementation Phases

### Phase 4.1: Single-Session Attach with Rendering

**Goal:** Replace the broken `tmax attach` with a working terminal client that correctly renders output, forwards input, handles resize, and supports detach.

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

### Phase 4.2: Rendering Polish

**Goal:** Correct rendering for real-world terminal output (true color, wide characters) and integration tests.

**Tasks:**
- [x] Implement true color support: map `vt100` cell colors to crossterm colors (16, 256, and RGB)
- [x] Implement Unicode/wide character support: use `unicode-width` for correct column alignment of CJK characters
- [x] Handle edge cases: very small terminal sizes (minimum viable: 40x10)
- [x] Add `Ctrl+Space, ?` - help overlay showing keybindings (detach, double-tap to send literal)
- [x] Add unit tests: true color, 256-color, wide chars, emoji, bold/dim SGR rendering
- [x] Remove dead multi-pane offset code from renderer

**Acceptance Criteria:**
- True color output renders correctly (16, 256, RGB)
- Wide characters (CJK, emoji) display with proper column alignment
- Small terminals show "terminal too small" instead of crashing
- Integration tests cover the attach/render/input/detach lifecycle

## File Structure

```
crates/tmax-client/
├── Cargo.toml
├── src/
│   ├── main.rs           # CLI args (clap), connect, launch event loop
│   ├── connection.rs     # ServerConnection (split async read/write)
│   ├── terminal.rs       # Terminal setup/teardown (raw mode, alternate screen)
│   ├── event_loop.rs     # Main tokio::select! loop
│   ├── renderer.rs       # Differential rendering: vt100 screen -> crossterm output
│   ├── keybindings.rs    # Prefix key state machine, action dispatch
│   └── status_bar.rs     # Bottom status bar rendering
```

## Keybinding Reference

All keybindings use `Ctrl+Space` as the prefix key.

| Keybinding | Action | Phase |
|-----------|--------|-------|
| `Ctrl+Space, d` | Detach | 4.1 |
| `Ctrl+Space, Ctrl+Space` | Send literal Ctrl+Space to PTY | 4.1 |
| `Ctrl+Space, ?` | Help overlay | 4.2 |

## Technical Considerations

### Rendering Strategy

The client maintains a `vt100::Parser`. On output:
1. Feed raw bytes to `parser.process(bytes)`
2. Call `parser.screen().contents_diff(&prev_screen)` to get only changed cells
3. For each changed cell, emit crossterm commands: `MoveTo(x, y)`, `SetColors(...)`, `Print(char)`
4. Save current screen as `prev_screen` for next diff

This is significantly more efficient than full-screen redraws.

### Prefix Key State Machine

```
State::Normal -> Ctrl+Space -> State::Prefix
State::Prefix -> recognized key -> execute action, -> State::Normal
State::Prefix -> unrecognized key -> forward keystroke, -> State::Normal
State::Prefix -> timeout (2s) -> -> State::Normal
```

## Risk Analysis

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| vt100 crate doesn't handle some escape sequences | Low | Medium | Fall back to raw vte if needed; vt100 is well-maintained |
| Wide character column calculation off | Medium | Low | Use `unicode-width` crate; test with CJK content |

## Success Metrics

- `tmax-attach <session>` renders output identically to directly running the command in a terminal
- Full terminal resize completes in < 50ms
- No visual artifacts during rapid output (e.g., `yes | head -1000`)
- True color apps (bat, delta, starship) render correctly

## References

- [crossterm 0.29 docs](https://docs.rs/crossterm/0.29)
- [vt100 0.16 docs](https://docs.rs/vt100/0.16)
- [unicode-width docs](https://docs.rs/unicode-width)
- Existing client code: `crates/tmax-cli/src/client.rs`, `crates/tmax-cli/src/commands.rs:261` (attach)
- Brainstorm: `docs/brainstorms/2026-02-14-tmax-terminal-multiplexer-brainstorm.md` (lines 153-208)
- Parent plan: `docs/plans/2026-02-14-feat-tmax-terminal-multiplexer-plan.md` (lines 591-618)
