---
title: "feat: tmax - Programmable Terminal Multiplexer for AI Workflows"
type: feat
status: active
date: 2026-02-14
brainstorm: docs/brainstorms/2026-02-14-tmax-terminal-multiplexer-brainstorm.md
---

# feat: tmax - Programmable Terminal Multiplexer for AI Workflows

## Overview

Build a ground-up terminal multiplexer in Rust designed as a programmable tool for AI agent workflows. tmax provides sandboxed, streamable terminal sessions with a JSON-lines control API over Unix sockets. It is agent-agnostic - orchestrators, agents, terminal clients, and web GUIs are all external consumers of the same API.

**Primary use case:** An orchestrator agent runs in a tmax session. Humans interact with it via a web GUI (edit attachment). The orchestrator creates sandboxed sub-agent sessions, monitors their output via event streams, and manages their lifecycle. Humans watch sub-agent sessions (view attachments) in the same GUI via xterm.js, seeing identical output to a native terminal.

## Problem Statement

Current terminal multiplexers (tmux, zellij) were designed for human-interactive use. They lack:
- A structured, programmatic API for creating/controlling/streaming sessions
- Filesystem sandboxing per session
- Multi-subscriber real-time event streaming
- A web-native bridge for browser-based monitoring
- Session nesting with inherited sandbox scope rules

AI agent workflows need a terminal execution layer that is programmable first, human-viewable second.

## Proposed Solution

A Rust workspace with a library-first architecture (`libtmax`) where multiple clients (CLI, terminal UI, web bridge) consume the same core. Sessions are composable primitives - independent of worktrees, agents, or each other. Sandbox scope, git worktree association, and nesting are all optional, orthogonal configurations.

## Technical Approach

### Architecture

```
tmax/
  Cargo.toml                    # workspace root
  crates/
    tmax-protocol/              # shared types, message definitions
    libtmax/                    # core library: PTY, sessions, scrollback, events
    tmax-server/                # daemon: manages sessions, listens on Unix socket
    tmax-cli/                   # CLI: create, list, kill, attach, stream
    tmax-web/                   # WebSocket bridge for web clients
    tmax-sandbox/               # OS-native filesystem sandboxing
    tmax-git/                   # git worktree auto-detection and convenience commands
    tmax-client/                # full terminal UI (Phase 4)
```

### Key Crate Versions (as of 2026-02)

| Crate | Version | Notes |
|-------|---------|-------|
| `portable-pty` | 0.9.0 | Watch master FD lifecycle - don't drop write side while reading |
| `crossterm` | 0.29.0 | Raw mode, mouse capture, alternate screen |
| `tokio` | latest | Async runtime |
| `axum` | 0.8.8 | Built-in WebSocket support (no separate tokio-tungstenite needed) |
| `serde` + `serde_json` | latest | JSON serialization |
| `memmap2` | latest | Optional history log backing (not on live path) |
| `git2` | latest | Git operations |
| `nix` | latest | Linux namespace syscalls |
| `vte` | latest | ANSI escape sequence parser for server-side VT state |
| `regex` | latest | Scrollback search |

### Implementation Phases

#### Phase 0: Core Engine

**Goal:** A working server that creates sessions, runs processes in PTYs, streams output to multiple subscribers, and supports edit/view attachments - all via CLI.

**Crates:** `tmax-protocol`, `libtmax`, `tmax-server`, `tmax-cli`

#### Phase 1: Web Bridge

**Goal:** Stream sessions to a browser via WebSocket + xterm.js.

**Crate:** `tmax-web`

#### Phase 2: Sandboxing

**Goal:** OS-native filesystem isolation per session.

**Crate:** `tmax-sandbox`

#### Phase 3: Git Integration

**Goal:** Auto-detect worktrees, display in pane borders, convenience commands.

**Crate:** `tmax-git`

#### Phase 4: Terminal Client

**Goal:** Single-session native terminal attach client with scrollback, search, and mouse support.

**Crate:** `tmax-client`

---

## Phase 0: Core Engine (Detailed)

### 0.1 - Workspace Setup and Protocol Definition

**`tmax-protocol/`** - Shared types used by all other crates.

```rust
// crates/tmax-protocol/src/lib.rs

/// Client-to-server requests
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    // Session management
    SessionCreate {
        exec: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        label: Option<String>,
        sandbox: Option<SandboxConfig>,
        parent_id: Option<SessionId>,
        cols: u16,
        rows: u16,
    },
    SessionDestroy { session_id: SessionId },
    SessionList,
    SessionTree,
    SessionInfo { session_id: SessionId },

    // Attachments
    Attach {
        session_id: SessionId,
        mode: AttachMode,
    },
    Detach { session_id: SessionId },

    // I/O
    SendInput {
        session_id: SessionId,
        data: Vec<u8>,
    },
    Resize {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },

    // Markers
    MarkerInsert {
        session_id: SessionId,
        name: String,
    },
    MarkerList { session_id: SessionId },

    // Event streaming
    Subscribe { session_id: SessionId },
    Unsubscribe { session_id: SessionId },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok { data: Option<serde_json::Value> },
    Error { message: String, code: ErrorCode },
    Event(Event),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Output {
        session_id: SessionId,
        data: Vec<u8>,
    },
    SessionCreated {
        session_id: SessionId,
        label: Option<String>,
    },
    SessionExited {
        session_id: SessionId,
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    SessionDestroyed {
        session_id: SessionId,
    },
    MarkerInserted {
        session_id: SessionId,
        name: String,
        offset: u64,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AttachMode {
    Edit,
    View,
}

pub type SessionId = String;
```

**Tasks:**
- [x] Initialize Rust workspace with `Cargo.toml` at root
- [x] Create `tmax-protocol` crate with all message types
- [x] Define `Request`, `Response`, `Event`, `AttachMode`, `SandboxConfig` types
- [x] Add `serde` serialization with `#[serde(tag = "cmd")]` for clean JSON
- [x] Write unit tests for serialization round-trips
- [x] Ensure all types are `Clone + Debug + Serialize + Deserialize`

### 0.2 - Core Library (libtmax)

**`libtmax/`** - PTY management, session lifecycle, scrollback, event broadcasting.

**Session Manager:**
```rust
// crates/libtmax/src/session.rs

pub struct SessionManager {
    sessions: HashMap<SessionId, Session>,
    session_tree: HashMap<SessionId, Vec<SessionId>>, // parent -> children
}

pub struct Session {
    pub id: SessionId,
    pub master_pty: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub scrollback: ScrollbackBuffer,
    pub metadata: SessionMetadata,
    pub event_tx: broadcast::Sender<Event>,
    pub attachments: Vec<Attachment>,
}

pub struct SessionMetadata {
    pub label: Option<String>,
    pub exec: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub sandbox: Option<SandboxConfig>,
    pub parent_id: Option<SessionId>,
    pub created_at: SystemTime,
}

pub struct Attachment {
    pub id: String,
    pub mode: AttachMode,
    pub created_at: SystemTime,
}
```

**Output Buffer (dual-layer):**
```rust
// crates/libtmax/src/output.rs

/// Live streaming layer: in-memory circular buffer of sequenced chunks.
/// Fast hot path for real-time streaming and reconnect catch-up.
pub struct LiveBuffer {
    chunks: VecDeque<OutputChunk>,
    max_chunks: usize,        // configurable cap
    next_seq: u64,             // global monotonic sequence ID
}

pub struct OutputChunk {
    pub seq: u64,              // global sequence ID for cursor tracking
    pub data: Vec<u8>,         // raw PTY bytes
    pub timestamp: SystemTime,
}

/// Per-client cursor for catch-up and backpressure.
pub struct ClientCursor {
    pub last_seq_seen: u64,
}

/// Optional durable history layer: append-only file for long replay/audit.
/// NOT on the live streaming hot path.
pub struct HistoryLog {
    file: File,
    write_pos: u64,
}

/// Markers stored separately, indexed by sequence ID.
pub struct Marker {
    pub name: String,
    pub seq: u64,
    pub timestamp: SystemTime,
}
```

**Server-side VT State (authoritative terminal state per pane):**
```rust
// crates/libtmax/src/vt_state.rs

/// Authoritative virtual terminal state maintained server-side.
/// Enables snapshot+delta recovery for reconnecting clients
/// instead of replaying all output chunks.
pub struct VtState {
    pub screen: Vec<Vec<Cell>>,   // current screen buffer (rows x cols)
    pub cursor: CursorPosition,
    pub scrollback: Vec<Vec<Cell>>,  // scrolled-off lines
    pub seq_at_snapshot: u64,     // seq ID when this snapshot was taken
}

pub struct Cell {
    pub char: char,
    pub attrs: CellAttributes,   // fg, bg, bold, etc.
}

/// Recovery protocol:
/// 1. Client connects with last_seq (or none)
/// 2. If last_seq is within LiveBuffer range: replay deltas from last_seq
/// 3. If last_seq is too old (gap exceeds ring): send fresh VtState snapshot
///    at current seq, then stream deltas from there
/// 4. New clients always get snapshot first
```

**Event Broker (fan-in/fan-out, keyed by session_id):**
```rust
// crates/libtmax/src/broker.rs

/// Central broker that manages per-session broadcast channels
/// and per-client subscription state.
pub struct EventBroker {
    /// Per-session broadcast channels (independent rings prevent
    /// one noisy session from starving others)
    channels: HashMap<SessionId, broadcast::Sender<Event>>,
}

/// Per-client subscription state across multiple sessions.
/// A single web client watching 10 agent sessions has 10 cursors.
pub struct ClientSubscriptions {
    pub cursors: HashMap<SessionId, ClientCursor>,
}

impl EventBroker {
    /// Subscribe a client to a session. Returns receiver + replays
    /// missed chunks from LiveBuffer since client's last_seq.
    pub fn subscribe(
        &self,
        session_id: &SessionId,
        last_seq: Option<u64>,
    ) -> (broadcast::Receiver<Event>, Vec<OutputChunk>) { ... }
}
```

**Backpressure & batching (for web bridge):**
```rust
// crates/tmax-web/src/ws.rs

/// Per-client backpressure policy:
/// - Coalesce output frames per tick interval (16-50ms)
/// - If client falls too far behind, skip to latest (don't buffer unbounded)
/// - Per-session and per-client quotas to prevent resource exhaustion
pub struct WebClientState {
    pub subscriptions: ClientSubscriptions,
    pub batch_interval_ms: u64,  // coalesce sends across sessions per tick
    pub max_lag_chunks: u64,     // disconnect/recover if client falls behind
}
```

**PTY I/O Loop:**
```rust
// Per-session async task that:
// 1. Reads from PTY master in a loop
// 2. Writes to scrollback buffer
// 3. Broadcasts Output events to all subscribers
// 4. Handles process exit -> broadcasts SessionExited
```

**Tasks:**
- [x] Create `libtmax` crate depending on `tmax-protocol`
- [x] Implement `SessionManager` with create/destroy/list/get operations
- [x] Implement PTY spawning via `portable-pty` (arbitrary exec, not shell)
- [x] Implement async PTY I/O loop (tokio task per session, reads PTY -> writes to LiveBuffer -> broadcasts events)
- [x] Implement `LiveBuffer` - in-memory circular buffer of sequenced `OutputChunk`s with configurable cap
- [x] Implement global monotonic sequence IDs (`seq`) on every output chunk
- [x] Implement `ClientCursor` for per-subscriber catch-up (client sends `last_seq_seen`, server replays from there)
- [ ] Implement optional `HistoryLog` - append-only file for long replay/audit (not on live hot path)
- [x] Implement `Marker` storage indexed by sequence ID
- [x] Implement `EventBroker` - central fan-in/fan-out keyed by session_id, independent broadcast channel per session
- [ ] Implement `VtState` - server-side authoritative VT state per pane using `vte` crate for ANSI parsing
- [ ] Implement snapshot+delta recovery: new/reconnecting clients get VtState snapshot then deltas
- [x] Implement resync: if client's last_seq exceeds ring, force fresh snapshot
- [x] Implement attachment tracking (edit/view mode per attachment)
- [x] Implement input ownership: only one edit attachment can send input at a time per session (input-lock). Second edit attachment queues or is rejected.
- [x] Implement session nesting (parent-child tree, cascade kill option)
- [x] Implement session metadata (label, tags, exec info)
- [x] Implement PTY resize forwarding
- [x] Implement backpressure handling for slow subscribers (drop/compact, don't block fast path)
- [x] Write integration tests: create session, read output, verify events with correct seq IDs
- [x] Write tests: concurrent subscribers receive same events
- [x] Write tests: client reconnect catches up from last_seq_seen
- [x] Write tests: session nesting and cascade destroy

### 0.3 - Server Daemon (tmax-server)

**`tmax-server/`** - Unix socket listener, client connection handler, routes requests to `SessionManager`.

**Server Architecture:**
```rust
// crates/tmax-server/src/main.rs

// 1. Parse config (TOML)
// 2. Create SessionManager
// 3. Bind UnixListener at /tmp/tmax-{uid}.sock (or XDG_RUNTIME_DIR)
// 4. Accept loop: spawn task per client connection
// 5. Each client task:
//    a. Read JSON-lines requests via LinesCodec
//    b. Route to SessionManager
//    c. For Subscribe: forward broadcast events to client as JSON-lines
//    d. For Attach: track attachment, forward output stream
//    e. For SendInput (edit mode only): write to session PTY
```

**Client Connection Handler:**
```rust
// crates/tmax-server/src/connection.rs

// Uses tokio_util::codec::LinesCodec for JSON-lines framing
// Split socket into read/write halves
// Read half: parse Request, dispatch to SessionManager
// Write half: send Response + forwarded Events
// Handle disconnect gracefully (remove subscriptions, detach)
```

**Tasks:**
- [x] Create `tmax-server` crate depending on `libtmax` and `tmax-protocol`
- [x] Implement socket path resolution (`$XDG_RUNTIME_DIR/tmax.sock` or `/tmp/tmax-{uid}.sock`)
- [x] Implement Unix socket listener with `tokio::net::UnixListener`
- [x] Implement client connection handler with `LinesCodec` for JSON-lines framing
- [x] Implement request routing: parse JSON -> match Request variant -> call SessionManager
- [x] Implement subscription forwarding: when client subscribes, spawn task that reads from session's broadcast channel and writes events to client socket
- [x] Implement attachment enforcement: reject SendInput for view-mode attachments
- [x] Implement graceful client disconnect (cleanup subscriptions, detach)
- [x] Implement server shutdown (kill all sessions or orphan based on config)
- [x] Implement TOML config loading (socket path, default session settings)
- [x] Implement PID file for single-instance enforcement
- [ ] Write integration tests: start server, connect client, create session, receive output
- [ ] Write tests: multiple clients subscribing to same session
- [ ] Write tests: edit vs view attachment enforcement

### 0.4 - CLI Tool (tmax-cli)

**`tmax-cli/`** - Human and agent-friendly CLI that connects to `tmax-server` via Unix socket.

**Commands:**
```
tmax server start           # Start the daemon (foreground or background)
tmax server stop            # Stop the daemon
tmax server status          # Check if daemon is running

tmax new [--exec CMD]       # Create session, default: exec required
         [--shell]          # Shorthand for --exec $SHELL
         [--label NAME]     # Label the session
         [--sandbox-write DIR]...  # Writable paths (repeatable)
         [--no-sandbox]     # Disable sandboxing
         [--parent ID]      # Nest under parent session
         [--cols N --rows N]  # PTY dimensions

tmax list                   # List all sessions
tmax list --tree            # Show session hierarchy
tmax info <session>         # Session details

tmax attach <session>       # Attach (edit mode, streams to terminal)
         [--view]           # View-only attachment
tmax detach                 # Detach current attachment

tmax send <session> <input> # Send input to session PTY
tmax resize <session> <cols> <rows>  # Resize PTY

tmax kill <session>         # Kill session
         [--cascade]        # Kill children too

tmax marker <session> <name>  # Insert marker
tmax markers <session>        # List markers

tmax stream <session>       # Stream raw output to stdout (for piping)
tmax subscribe <session>    # Stream JSON events to stdout
```

**Tasks:**
- [x] Create `tmax-cli` crate with `clap` for argument parsing
- [x] Implement Unix socket client connection (find server socket, connect)
- [x] Implement all commands: `new`, `list`, `info`, `attach`, `send`, `kill`, `marker`, `stream`, `subscribe`
- [x] Implement `tmax attach` with raw terminal mode (forward stdin/stdout bidirectionally)
- [x] Implement `tmax stream` for raw byte piping (useful for orchestrators)
- [x] Implement `tmax subscribe` for JSON event stream (useful for orchestrators)
- [x] Implement `tmax server start` (spawn daemon process, daemonize)
- [x] Implement pretty-printed output for `list`, `info`, `markers`
- [x] Implement `--tree` output for session hierarchy
- [x] Handle server-not-running errors gracefully with helpful messages
- [ ] Write CLI integration tests using assert_cmd

---

## Phase 1: Web Bridge (Detailed)

### 1.1 - WebSocket Bridge (tmax-web)

**`tmax-web/`** - Axum HTTP server that bridges tmax-server to WebSocket clients.

**Endpoints:**
```
GET  /api/sessions          # List sessions (JSON)
GET  /api/sessions/:id      # Session details (JSON)
GET  /api/sessions/tree     # Session hierarchy (JSON)
WS   /ws/session/:id        # WebSocket: raw PTY byte stream + input
     ?mode=edit|view         # Attachment mode (query param)
```

**WebSocket Protocol:**
- **Server -> Client:** Raw binary frames (PTY output bytes) for xterm.js
- **Client -> Server (edit mode only):** Binary frames (keyboard input bytes)
- **Client -> Server:** JSON text frames for control messages (resize, markers)
- **Server -> Client:** JSON text frames for events (session exited, marker inserted)

**Architecture:**
```rust
// tmax-web connects to tmax-server via Unix socket
// Each WebSocket connection = one tmax subscription + one attachment
// Binary frames: raw PTY bytes (bidirectional in edit mode)
// Text frames: JSON control messages
```

**Tasks:**
- [x] Create `tmax-web` crate with `axum` 0.8.8
- [x] Implement REST endpoints for session listing and info
- [x] Implement WebSocket upgrade handler with edit/view mode from query param
- [x] Implement multi-session WebSocket: one WS connection can subscribe to multiple sessions (client sends subscribe/unsubscribe messages)
- [x] Implement binary frame streaming: subscribe to session events via EventBroker, forward Output data as binary WebSocket frames tagged with session_id
- [x] Implement frame batching: coalesce output across sessions per tick (16-50ms configurable) to reduce WebSocket frame overhead
- [x] Implement per-client backpressure: if client falls behind max_lag_chunks, skip to latest (don't buffer unbounded)
- [ ] Implement per-session and per-client quotas to prevent resource exhaustion
- [x] Implement reconnect catch-up: client sends last_seq per session, server replays from LiveBuffer
- [x] Implement input forwarding: receive binary frames from client, forward as SendInput to tmax-server (edit mode only)
- [x] Implement resize handling: receive JSON text frame with cols/rows, forward to tmax-server
- [x] Implement CORS configuration for Next.js dev server
- [x] Implement connection lifecycle: attach on connect, detach on disconnect, cleanup all subscriptions
- [x] Write integration tests: WebSocket connects, subscribes to multiple sessions, receives interleaved output
- [x] Write tests: reconnect catch-up replays correct chunks
- [ ] Write tests: backpressure correctly skips to latest for slow clients

---

## Phase 2: Sandboxing (Detailed)

### 2.1 - Linux Sandboxing

**Strategy:** User namespaces + mount namespaces. No root required.

```
1. Before spawning PTY process:
2. unshare(CLONE_NEWUSER | CLONE_NEWNS)
3. Remount / as read-only
4. Bind-mount each writable path as read-write
5. Spawn the process inside the namespace
```

**Tasks:**
- [x] Create `tmax-sandbox` crate
- [ ] Implement Linux sandbox using `nix` crate (unshare, mount, bind-mount)
- [x] Implement writable path configuration (primary scope + shared dirs)
- [x] Implement nesting enforcement: validate child writable paths are subsets of parent
- [x] Test: process can write inside sandbox scope
- [x] Test: process CANNOT write outside sandbox scope
- [x] Test: nested session scope validation

### 2.2 - macOS Sandboxing

**Strategy:** Apple Containerization framework on macOS 26+ (Tahoe). Fallback to warning on older versions.

**Tasks:**
- [ ] Research Apple Containerization framework API for Rust FFI
- [x] Implement macOS sandbox using Containerization (or sandbox-exec as interim)
- [x] Implement graceful fallback: warn user on unsupported macOS versions
- [x] Test: equivalent isolation to Linux implementation

---

## Phase 3: Git Integration (Detailed)

**Tasks:**
- [ ] Create `tmax-git` crate using `git2`
- [ ] Implement auto-detection: on session create, detect if cwd is in a git repo or worktree
- [ ] Store detected git info in session metadata (branch, worktree path, repo root)
- [ ] Implement `tmax new --worktree <branch>` shorthand (create worktree via git2, set as session cwd)
- [ ] Implement `tmax worktree clean <session>` (remove worktree + kill session)
- [ ] Expose git metadata via session info API (for pane border display)
- [ ] Implement dirty/clean state detection for display

---

## Phase 4: Terminal Client (Detailed)

### 4.1 - Single-Session Attach with Rendering

**Tasks:**
- [x] Create `tmax-client` crate using `crossterm` and `vt100`
- [x] Implement virtual terminal rendering with differential updates
- [x] Implement status bar with session info
- [x] Implement Ctrl+Space prefix key system with detach
- [x] Implement raw terminal input forwarding to PTY
- [x] Implement terminal resize handling

### 4.2 - Rendering Polish

**Tasks:**
- [ ] Implement true color support (16, 256, RGB)
- [ ] Implement Unicode/wide character support (CJK, emoji)
- [ ] Handle small terminal edge cases
- [ ] Integration tests for attach/render/input/detach lifecycle

---

## Alternative Approaches Considered

### Wrap tmux
tmux's protocol was designed for human interaction, not structured programmatic control. Bolting a JSON API on top creates a leaky abstraction. The streaming, sandboxing, and nesting features would require invasive tmux patches.

### Fork Zellij
Zellij's WASM plugin system adds ~80MB baseline memory. Its architecture is optimized for human-interactive use with plugin extensibility, not programmatic API-first control. Forking would mean maintaining divergent code against upstream.

### Use Docker/containers for sandboxing
Heavyweight for per-session isolation. Container startup time and resource overhead are too high for ephemeral agent sessions that may run for seconds. User namespaces give equivalent filesystem isolation without container overhead.

## Acceptance Criteria

### Functional Requirements

- [ ] Create sessions that run arbitrary executables in PTYs
- [ ] Stream PTY output to multiple concurrent subscribers in real time
- [ ] Edit attachments can send input; view attachments cannot
- [ ] Sessions can be nested with parent-child tracking
- [ ] CLI can create, list, kill, attach, stream, and subscribe
- [ ] WebSocket bridge streams raw PTY bytes to xterm.js
- [ ] Filesystem sandboxing isolates session writes to declared paths
- [ ] Nested sessions inherit and narrow sandbox scope
- [ ] Git worktree auto-detection and display in session metadata
- [ ] Terminal client renders a single session with scrollback and search

### Non-Functional Requirements

- [ ] Session creation < 100ms
- [ ] Output streaming latency < 10ms from PTY to subscriber
- [ ] Memory per session < 5MB (excluding scrollback)
- [ ] Single binary distribution (per platform)
- [ ] Works on Linux (x86_64, aarch64) and macOS (aarch64)

### Quality Gates

- [ ] All phases have integration tests
- [ ] Protocol types have serialization round-trip tests
- [ ] Sandbox has escape-attempt tests (symlinks, path traversal)
- [ ] CI runs on Linux and macOS

### Testing Strategy

- [ ] **Golden VT tests:** Feed known escape sequences through VtState parser, assert screen buffer matches expected output. Ensures terminal emulation correctness.
- [ ] **Load tests:** Many panes + slow clients. Verify backpressure kicks in, no unbounded memory growth, fair scheduling.
- [ ] **Deterministic replay tests:** Capture output logs from real sessions, replay through VtState, assert deterministic screen state. Regression-proof.
- [ ] **Snapshot+delta recovery tests:** Disconnect client, reconnect at various seq gaps, verify correct state restoration.
- [ ] **Input ownership tests:** Multiple edit attachments on same session, verify input-lock semantics.

## Dependencies & Prerequisites

- Rust toolchain (stable)
- Linux kernel 3.8+ for user namespaces (sandboxing)
- macOS 26+ for Apple Containerization (sandboxing, Phase 2)
- Node.js for xterm.js proof-of-concept (Phase 1)

## Risk Analysis & Mitigation

| Risk | Impact | Mitigation |
|------|--------|------------|
| macOS sandbox-exec deprecated | Phase 2 delayed on macOS | Use Apple Containerization on macOS 26+; warn on older versions |
| History log disk I/O | Slows live path if not decoupled | History log is append-only, off the live streaming hot path. Live buffer is pure in-memory VecDeque. |
| portable-pty master FD lifecycle | Session hangs or crashes | Keep master handle alive, don't drop write side while reading |
| Terminal UI complexity (Phase 4) | Delayed or poor quality | Scoped to single-session client; web bridge handles multi-session; uses `vt100` crate for parsing |
| Broadcast channel lagging | Slow subscribers miss output | Handle `Lagged` errors gracefully, log warning, buffer size tunable |

## References & Research

### Internal References
- Brainstorm: `docs/brainstorms/2026-02-14-tmax-terminal-multiplexer-brainstorm.md`

### External References
- [portable-pty docs](https://docs.rs/portable-pty/0.9.0)
- [crossterm docs](https://docs.rs/crossterm/0.29.0)
- [axum WebSocket](https://docs.rs/axum/0.8.8/axum/extract/ws/index.html)
- [tokio::sync::broadcast](https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html)
- [memmap2 docs](https://docs.rs/memmap2)
- [Zellij architecture](https://github.com/zellij-org/zellij)
- [Linux user namespaces](https://man7.org/linux/man-pages/man7/user_namespaces.7.html)
- [Apple Containerization](https://developer.apple.com/documentation/containerization)
- [@xterm/xterm 6.0](https://github.com/xtermjs/xterm.js) (scoped package, old `xterm` name deprecated)
