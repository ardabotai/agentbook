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
    tmax-local/                # daemon: manages sessions, listens on Unix socket
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

**Crates:** `tmax-protocol`, `libtmax`, `tmax-local`, `tmax-cli`

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

**Goal:** Full native terminal UI competitive with tmux/zellij.

**Crate:** `tmax-client`

---

## Progress Log

- **2026-02-14 (Phase 0 baseline):** Rust workspace scaffolded, protocol crate implemented with limits + serialization tests, libtmax PTY/session engine implemented, server daemon and CLI implemented, and core tests passing.
- **2026-02-14 (Phase 0 hardening):** Added `SessionExited` emission, bounded output chunking, non-blocking outbound queue policy in server, and additional core tests (reconnect from cursor, cascade destroy, single edit attachment).
- **2026-02-14 (Phase 1 baseline):** Implemented `tmax-web` REST session APIs and single-session WebSocket bridge (`/ws/session/:id`) forwarding output/input/control over the Unix socket protocol.
- **2026-02-14 (Phase 2 baseline):** Implemented `tmax-sandbox` scope normalization and nested scope enforcement, and wired parent-child sandbox subset validation into session creation.
- **2026-02-14 (Phase 3 baseline):** Implemented `tmax-git` metadata detection (repo/worktree/branch/dirty) and surfaced git metadata via session summary/info responses.
- **2026-02-14 (Phase 4 baseline):** Replaced placeholder `tmax-client` with a minimal native client using `crossterm` for alternate screen + raw input forwarding in edit mode.
- **2026-02-14 (Phase 1 hardening):** Added configurable frame batching, output backpressure/drop notices, per-connection frame/input/control quotas, and CORS allow-origin configuration in `tmax-web`.
- **2026-02-14 (Phase 3 commands):** Added git worktree lifecycle helpers and wired CLI `tmax new --worktree <branch>` and `tmax worktree clean <session>`.
- **2026-02-14 (Phase 0 integration tests):** Added `tmax-local` end-to-end integration tests for create/stream, multi-client subscription replay, edit-vs-view attachment enforcement, and one-client multi-session subscription framing.
- **2026-02-14 (Phase 0 CLI integration tests):** Added `tmax-cli` integration tests with a mock Unix-socket server for `list`/`new` protocol behavior and argument validation (`new` without `--exec/--shell`).
- **2026-02-14 (Phase 1 integration tests):** Added end-to-end `tmax-web` WebSocket tests for ordered binary output and reconnect catch-up (`last_seq`), and fixed Axum 0.8 route syntax (`{id}`).
- **2026-02-14 (Phase 0 socket/auth tests):** Added socket-mode (`0600`) integration coverage and peer-uid accept/reject unit tests for local auth enforcement.
- **2026-02-14 (Phase 4 VT foundation):** Added a `vte`-based virtual terminal screen buffer in `tmax-client` and switched rendering from raw output passthrough to parsed screen-state rendering.
- **2026-02-14 (Phase 4 layout foundation):** Added pane layout primitives in `tmax-client` for horizontal/vertical splits and split-ratio resize with unit tests.
- **2026-02-14 (Phase 4 interaction hardening):** Added pane border rendering with metadata (`[EDIT]/[VIEW]`, git, sandbox), Ctrl+Space prefix commands (split/cycle), terminal resize handling, and mouse-wheel input forwarding in `tmax-client`.
- **2026-02-14 (Phase 2 scope hardening):** Added canonicalized path handling (including symlink resolution) in `tmax-sandbox` and escape-attempt test coverage for symlink traversal outside parent scope.
- **2026-02-14 (Transport rejection tests):** Added oversized input rejection test in `libtmax` and surfaced explicit `sandboxed` metadata in `SessionSummary` for clients.
- **2026-02-14 (Phase 2 macOS runtime sandbox):** Added `sandbox-exec` runtime wrapping for sandboxed session spawns on macOS with tests demonstrating allowed-inside and blocked-outside writes.
- **2026-02-14 (Phase 2 Linux runtime wiring):** Added explicit Linux unshare/mount namespace command wrapping for sandboxed spawns to avoid silent unsandboxed fallback.
- **2026-02-14 (CI baseline):** Added GitHub Actions Linux/macOS matrix with `fmt`, strict `clippy -D warnings`, and workspace tests.
- **2026-02-14 (Phase 0 history log):** Added optional append-only `HistoryLog` capture in `libtmax` (via `TMAX_HISTORY_DIR`) with durability test coverage.
- **2026-02-14 (Phase 0 metadata tags):** Added session tags through protocol/server/CLI and surfaced them in `SessionSummary`.
- **2026-02-14 (Backpressure hardening):** Confirmed non-blocking server outbound queues (`try_send` disconnect policy), lag-tolerant broadcast receivers, and web-side drop/compact behavior with tests.
- **2026-02-14 (Phase 0 VT snapshot/recovery):** Added server-side `VtState` maintenance with snapshot events and reconnect resync logic (snapshot on new/stale cursors, delta replay on recent cursors) plus tests.
- **2026-02-14 (Phase 0 event broker):** Added `EventBroker` with per-session channel registration/removal/subscription and wired session channel lifecycle through it.
- **2026-02-14 (Replay/stability hardening):** Added deterministic VT replay tests and PTY-open retry/backoff in session creation to reduce transient allocation failures under load.
- **2026-02-14 (Load coverage):** Promoted multi-subscriber load/backpressure test into normal test runs and validated stability under full workspace test execution.
- **2026-02-14 (Phase 2 Linux runner + parity):** Added `tmax-sandbox-runner` Linux namespace/mount runner wiring, Linux isolation integration tests (inside write allowed / outside blocked, platform-gated), and macOS Containerization FFI research notes in `docs/research/2026-02-14-macos-containerization-ffi.md`.
- **2026-02-14 (Phase 4 keybinding + scroll UX):** Expanded `tmax-client` to support brainstorm keybindings (`c/n/p/1-9`, `|/-`, `h/j/k/l`, `d`, `/`, `m`, `w`, `?`), window switching, scroll mode, regex search highlight, marker jump navigation, and smooth mouse-wheel scrolling.
- **2026-02-14 (Perf + packaging gates):** Added `tmax-local` perf smoke integration gate (session create latency, PTY output latency, RSS delta reporting), release packaging script (`scripts/package-release.sh`) generating one artifact per platform, and fixed/enhanced CI workflow formatting with per-OS target compile checks.
- **2026-02-14 (Memory tuning):** Reduced per-session helper threads from two to one, lowered thread stack size, and reduced default live-buffer/broadcast capacities; memory improved but remains above the 5MB target in smoke runs.
- **2026-02-14 (Phase 3/4 integration completion):** Added `tmax-git` integration tests for metadata + worktree lifecycle and `tmax-client` headless protocol smoke integration test for session info/attach/detach flow.
- **2026-02-14 (Linux+macOS socket auth evidence):** Re-ran socket auth/permission tests on macOS locally and inside Linux (aarch64) container; both passed. x86_64 Linux runtime validation via emulation remains unstable due toolchain/compiler crashes under QEMU.
- **2026-02-14 (Ubuntu x86_64 production validation):** Ran full strict gates over SSH on native Ubuntu x86_64 (`cargo fmt --check`, strict `clippy -D warnings`, `cargo test --workspace`), fixed Linux-specific clippy/test issues discovered there, and revalidated green.
- **2026-02-14 (Memory gate closed on target host):** Perf smoke on native Ubuntu x86_64 reported `create_ms=11`, `stream_ms=0`, `rss_delta_kb=4992` in `integration_perf_smoke_create_and_stream_latency`.
- **2026-02-14 (RC1 packaging + runbook):** Built release artifacts for macOS aarch64 and Linux x86_64, generated `dist/SHA256SUMS.txt` + `dist/RELEASE-MANIFEST.toml`, and added deployment/monitoring release notes in `docs/releases/2026-02-14-rc1.md`.
- **2026-02-15 (Post-RC Step 1 complete):** Added production service assets under `ops/systemd/`, added `tmax-cli health` (protocol + session-list round-trip checks with non-zero unhealthy exit), added CLI integration tests for health success/failure, updated release packaging to include systemd assets, and documented systemd deployment in `docs/operations/systemd.md`.
- **2026-02-15 (Post-RC Step 2 complete):** Added idempotent Linux deploy/rollback automation scripts with health-gated failure behavior (`scripts/deploy-linux.sh`, `scripts/rollback-linux.sh`) and documented automated usage in operations docs.
- **2026-02-15 (Post-RC Step 3 partial):** Added CI package-content verification for release artifacts to ensure required service assets are always present in tarballs.
- **2026-02-15 (Step 3 host-run blocker):** Verified SSH connectivity to Ubuntu host and `systemctl` availability, but non-interactive `sudo` is not currently available (`sudo -n true` requires password), so full scripted system-level deploy/rollback evidence is pending an interactive sudo path.
- **2026-02-15 (Post-RC Step 4 complete):** Added `tmax-agent-sdk` crate with high-level `run_task`/`tail_task`/`cancel_task` APIs and wired `tmax-cli` commands (`run-task`, `tail-task`, `cancel-task`) so agents can run tasks without manual attach/subscription protocol choreography.
- **2026-02-15 (Post-RC Step 5 complete):** Expanded `tmax-agent-sdk` with structured failure classes, retry policies, timeout/cancel execution (`execute_task_and_collect`), resumable tail helper, readiness/health helpers, and deploy/rollback wrappers; wired retry+timeout flags into `tmax-cli` task commands.
- **2026-02-15 (Inter-agent messaging + shared task list):** Added first-class mailbox and shared task primitives end-to-end: protocol request/event additions (`message_send/list/ack/unread_count`, `task_create/list/claim/set_status`, `message_received`, `task_updated`), `libtmax` inbox/task state with dependency unblocking, server request routing, CLI command groups (`msg`, `tasks`), SDK helper APIs, and integration coverage in `libtmax`, `tmax-local`, `tmax-cli`, and `tmax-agent-sdk`.
- **2026-02-15 (Session-aware caller context for comms):** Added connection-scoped session awareness in `tmax-local` for mailbox/task endpoints: sender inference from bound attachments when possible, explicit-session enforcement for inbox/task calls on attached connections, and integration coverage for attached-session access control + sender inference.
- **2026-02-15 (Hierarchy-aware comms policy modes):** Added configurable server comms policy (`open`, `same_subtree`, `parent_only`) via CLI/config, enforced policy checks for mailbox sender->recipient routes and task claim/status actor-peer relations, and added integration coverage showing `parent_only` denies sibling message/task routes while allowing parent-child routes.

## Post-RC Production Hardening (Active)

### Step 1: Service Operations Baseline

**Goal:** Standardize Linux service deployment and add a first-class automated health gate.

**Tasks:**
- [x] Add hardened `systemd` unit + environment/config templates in `ops/systemd/`
- [x] Add `tmax-cli health` command for socket/connectivity/protocol/session-list round-trip validation
- [x] Add `tmax-cli` integration tests for healthy and unhealthy health-check paths
- [x] Include `ops/systemd` assets in release package output (`scripts/package-release.sh`)
- [x] Add operations deployment guide (`docs/operations/systemd.md`) and reference it from release notes

**Evidence:**
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- Smoke: `tmax-cli --socket <sock> health --json` after starting `tmax-local`

### Step 2: Deployment Automation Tightening

**Goal:** Make deploy/update flows less error-prone and fully scriptable.

**Tasks:**
- [x] Add an idempotent deploy script for Linux hosts (install/update symlink/systemd restart/health verify)
- [x] Add rollback command path using previous release symlink target
- [x] Add script-level smoke checks that fail fast on unhealthy status

**Evidence:**
- `bash -n scripts/deploy-linux.sh scripts/rollback-linux.sh`
- `scripts/deploy-linux.sh --artifact dist/tmax-<target>.tar.gz --dry-run`
- `scripts/rollback-linux.sh --install-root /tmp/tmax-roll-test --dry-run`

### Step 3: Production Verification Expansion

**Goal:** Close remaining operational confidence gaps with repeatable evidence.

**Tasks:**
- [ ] Run full package + deploy + health verification end-to-end on Ubuntu host using Step 2 automation
- [ ] Capture and document a rollback drill result in release notes
- [x] Add CI check that verifies packaged tarball contains expected ops assets

### Step 4: Agent-First Interface (Complete)

**Goal:** Raise agent ergonomics from protocol-level primitives to task-level operations.

**Tasks:**
- [x] Add `tmax-agent-sdk` crate with high-level task operations (`run_task`, `tail_task`, `cancel_task`)
- [x] Add SDK integration tests for task execution and cancellation request flow
- [x] Add high-level `tmax-cli` commands (`run-task`, `tail-task`, `cancel-task`) that remove attachment/subscription bookkeeping from agent callers
- [x] Add CLI integration tests for `run-task` and `cancel-task` flows
- [x] Update docs to expose agent-first command surface

**Evidence:**
- `cargo test -p tmax-agent-sdk`
- `cargo test -p tmax-cli --tests`

### Step 5: Agent Reliability APIs (Complete)

**Goal:** Make agent integrations robust under transient faults and long-running task behavior.

**Tasks:**
- [x] Add structured SDK error classes for connection/protocol/server/timeout/task/command failures
- [x] Add retry policy model + reusable async retry helper
- [x] Add `execute_task_and_collect` with timeout and cancel-on-timeout behavior
- [x] Add resumable tail helper with reconnect retries (`tail_task_resumable`)
- [x] Add readiness/ops helpers (`health`, `wait_ready`, `run_deploy`, `run_rollback`)
- [x] Wire timeout/retry flags into CLI task flows (`run-task`, `tail-task`)

**Evidence:**
- `cargo test -p tmax-agent-sdk`
- `cargo clippy -p tmax-agent-sdk --all-targets -- -D warnings`
- `cargo test -p tmax-cli --tests`
- `cargo clippy --workspace --all-targets -- -D warnings`

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
        last_seq_seen: Option<u64>,
    },
    Detach { attachment_id: AttachmentId },

    // I/O
    SendInput {
        session_id: SessionId,
        attachment_id: AttachmentId,
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
    Subscribe {
        session_id: SessionId,
        last_seq_seen: Option<u64>,
    },
    Unsubscribe { session_id: SessionId },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol_version: u32,
        features: Vec<String>,
    },
    Ok { data: Option<serde_json::Value> },
    Error { message: String, code: ErrorCode },
    Event(Event),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Output {
        session_id: SessionId,
        seq: u64,
        data_b64: String,
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
pub type AttachmentId = String;
```

**Tasks:**
- [x] Initialize Rust workspace with `Cargo.toml` at root
- [x] Create `tmax-protocol` crate with all message types
- [x] Define `Request`, `Response`, `Event`, `AttachMode`, `SandboxConfig` types
- [x] Add protocol handshake (`Response::Hello`) with explicit protocol version + feature flags
- [x] Define hard limits in protocol constants (`MAX_JSON_LINE_BYTES`, `MAX_OUTPUT_CHUNK_BYTES`, `MAX_INPUT_CHUNK_BYTES`)
- [x] Use attachment-scoped lifecycle: `Attach` returns `attachment_id`, `Detach` and `SendInput` require `attachment_id`
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
- [x] Chunk PTY output to bounded frame sizes (`MAX_OUTPUT_CHUNK_BYTES`) before event emission
- [x] Implement `LiveBuffer` - in-memory circular buffer of sequenced `OutputChunk`s with configurable cap
- [x] Implement global monotonic sequence IDs (`seq`) on every output chunk
- [x] Implement `ClientCursor` for per-subscriber catch-up (client sends `last_seq_seen`, server replays from there)
- [x] Implement optional `HistoryLog` - append-only file for long replay/audit (not on live hot path)
- [x] Implement `Marker` storage indexed by sequence ID
- [x] Implement `EventBroker` - central fan-in/fan-out keyed by session_id, independent broadcast channel per session
- [x] Implement `VtState` - server-side authoritative VT state per pane using `vte` crate for ANSI parsing
- [x] Implement snapshot+delta recovery: new/reconnecting clients get VtState snapshot then deltas
- [x] Implement resync: if client's last_seq exceeds ring, force fresh snapshot
- [x] Implement attachment tracking (edit/view mode per attachment, attachment_id lifecycle)
- [x] Implement input ownership: only one edit attachment can send input at a time per session (input-lock). Second edit attachment queues or is rejected.
- [x] Implement session nesting (parent-child tree, cascade kill option)
- [x] Implement session metadata (label, tags, exec info)
- [x] Implement PTY resize forwarding
- [x] Implement backpressure handling for slow subscribers (drop/compact, don't block fast path)
- [x] Write integration tests: create session, read output, verify events with correct seq IDs
- [x] Write tests: concurrent subscribers receive same events
- [x] Write tests: client reconnect catches up from last_seq_seen
- [x] Write tests: session nesting and cascade destroy

### 0.3 - Server Daemon (tmax-local)

**`tmax-local/`** - Unix socket listener, client connection handler, routes requests to `SessionManager`.

**Server Architecture:**
```rust
// crates/tmax-local/src/main.rs

// 1. Parse config (TOML)
// 2. Create SessionManager
// 3. Bind UnixListener at /tmp/tmax-{uid}.sock (or XDG_RUNTIME_DIR)
// 4. Accept loop: spawn task per client connection
// 5. Each client task:
//    a. Read JSON-lines requests via LinesCodec
//    b. Route to SessionManager
//    c. Push all outbound responses/events into a per-client bounded mpsc queue
//    d. Single writer task drains queue and writes JSON-lines to socket
//    e. For SendInput (edit mode only): write to session PTY
```

**Client Connection Handler:**
```rust
// crates/tmax-local/src/connection.rs

// Uses tokio_util::codec::LinesCodec for JSON-lines framing
// Split socket into read/write halves + bounded outbound queue
// Read half: parse Request, dispatch to SessionManager
// Subscription/attach tasks: enqueue outbound messages only (never write socket directly)
// Write half: single task sends Response + forwarded Events
// Handle disconnect gracefully (remove subscriptions, detach)
```

**Tasks:**
- [x] Create `tmax-local` crate depending on `libtmax` and `tmax-protocol`
- [x] Implement socket path resolution (`$XDG_RUNTIME_DIR/tmax.sock` or `/tmp/tmax-{uid}.sock`)
- [x] Enforce socket security: runtime dir `0700`, socket `0600`, remove stale socket on startup
- [x] Enforce local auth: verify peer credentials (`SO_PEERCRED`/`getpeereid`) match allowed uid policy
- [x] Implement Unix socket listener with `tokio::net::UnixListener`
- [x] Implement client connection handler with `LinesCodec` for JSON-lines framing
- [x] Implement request routing: parse JSON -> match Request variant -> call SessionManager
- [x] Implement per-client outbound `mpsc` queue + single writer task (no concurrent socket writers)
- [x] Implement subscription forwarding: when client subscribes, spawn task that reads from session's broadcast channel and enqueues events
- [x] Implement outbound queue backpressure policy (bounded queue + disconnect/drop strategy)
- [x] Benchmark JSON-lines output path against latency SLO; add binary fast-path milestone if SLO is not met
- [x] Implement attachment enforcement: reject SendInput for view-mode attachments
- [x] Implement graceful client disconnect (cleanup subscriptions, detach)
- [x] Implement server shutdown (kill all sessions or orphan based on config)
- [x] Implement TOML config loading (socket path, default session settings)
- [x] Implement PID file for single-instance enforcement
- [x] Write integration tests: start server, connect client, create session, receive output
- [x] Write tests: multiple clients subscribing to same session
- [x] Write tests: edit vs view attachment enforcement
- [x] Write tests: one client subscribed to many sessions does not interleave/corrupt JSON framing
- [x] Write tests: socket permissions and peer uid checks reject unauthorized client

### 0.4 - CLI Tool (tmax-cli)

**`tmax-cli/`** - Human and agent-friendly CLI that connects to `tmax-local` via Unix socket.

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
tmax detach <attachment>    # Detach specific attachment

tmax send <session> <input> # Send input to session PTY (requires edit attachment context)
         [--attachment ID]  # Explicit attachment ownership
tmax resize <session> <cols> <rows>  # Resize PTY

tmax kill <session>         # Kill session
         [--cascade]        # Kill children too

tmax marker <session> <name>  # Insert marker
tmax markers <session>        # List markers

tmax stream <session>       # Stream raw output to stdout (for piping)
tmax subscribe <session>    # Stream JSON events to stdout
tmax run-task ...           # Create+tail+wait task flow for agents
         [--timeout-ms N]   # Optional timeout with cancel-on-timeout
         [--retry-attempts N --retry-base-ms N --retry-max-ms N]  # Transient retry policy
tmax tail-task <session>    # Tail a task until exit (resumable retry capable)
         [--retry-attempts N --retry-base-ms N --retry-max-ms N]
tmax cancel-task <session>  # Cancel a task session
```

**Tasks:**
- [x] Create `tmax-cli` crate with `clap` for argument parsing
- [x] Implement Unix socket client connection (find server socket, connect)
- [x] Implement all commands: `new`, `list`, `info`, `attach`, `send`, `kill`, `marker`, `stream`, `subscribe`
- [x] Implement `tmax attach` with raw terminal mode (forward stdin/stdout bidirectionally)
- [x] Print returned `attachment_id` on attach and track current attachment context for detach/send
- [x] Implement `tmax stream` for raw byte piping (useful for orchestrators)
- [x] Implement `tmax subscribe` for JSON event stream (useful for orchestrators)
- [x] Implement `tmax server start` (spawn daemon process, daemonize)
- [x] Implement pretty-printed output for `list`, `info`, `markers`
- [x] Implement `--tree` output for session hierarchy
- [x] Handle server-not-running errors gracefully with helpful messages
- [x] Write CLI integration tests using assert_cmd

---

## Phase 1: Web Bridge (Detailed)

### 1.1 - WebSocket Bridge (tmax-web)

**`tmax-web/`** - Axum HTTP server that bridges tmax-local to WebSocket clients.

**Endpoints:**
```
GET  /api/sessions          # List sessions (JSON)
GET  /api/sessions/:id      # Session details (JSON)
GET  /api/sessions/tree     # Session hierarchy (JSON)
WS   /ws/session/:id        # WebSocket: raw PTY byte stream + input
     ?mode=edit|view&last_seq=123  # `last_seq` optional reconnect cursor
```

**WebSocket Protocol:**
- **Server -> Client:** Raw binary frames (PTY output bytes) for xterm.js
- **Client -> Server (edit mode only):** Binary frames (keyboard input bytes)
- **Client -> Server:** JSON text frames for control messages (resize, markers)
- **Server -> Client:** JSON text frames for events (session exited, marker inserted)
- **Session model:** one WebSocket connection maps to exactly one session; multi-pane UIs open multiple WS connections.

**Architecture:**
```rust
// tmax-web connects to tmax-local via Unix socket
// Each WebSocket connection = one tmax subscription + one attachment
// Binary frames: raw PTY bytes (bidirectional in edit mode)
// Text frames: JSON control messages + lifecycle events
```

**Tasks:**
- [x] Create `tmax-web` crate with `axum` 0.8.8
- [x] Implement REST endpoints for session listing and info
- [x] Implement WebSocket upgrade handler with edit/view mode from query param
- [x] Implement binary frame streaming: subscribe to session output, forward PTY bytes as raw binary WebSocket frames
- [x] Implement frame batching: coalesce output per connection tick (16-50ms configurable) to reduce WebSocket frame overhead
- [x] Implement per-client backpressure: if client falls behind max_lag_chunks, skip to latest (don't buffer unbounded)
- [x] Implement per-session and per-client quotas to prevent resource exhaustion
- [x] Implement reconnect catch-up: client sends `last_seq`, server replays from LiveBuffer
- [x] Implement input forwarding: receive binary frames from client, forward as SendInput to tmax-local (edit mode only)
- [x] Implement resize handling: receive JSON text frame with cols/rows, forward to tmax-local
- [x] Implement CORS configuration for Next.js dev server
- [x] Implement connection lifecycle: attach on connect, detach on disconnect, cleanup subscription/attachment state
- [x] Write integration tests: WebSocket connects to one session, receives ordered output frames
- [x] Write tests: reconnect catch-up replays correct chunks
- [x] Write tests: backpressure correctly skips to latest for slow clients

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
- [x] Implement Linux sandbox using `nix` crate (unshare, mount, bind-mount)
- [x] Implement writable path configuration (primary scope + shared dirs)
- [x] Implement nesting enforcement: validate child writable paths are subsets of parent
- [x] Test: process can write inside sandbox scope
- [x] Test: process CANNOT write outside sandbox scope
- [x] Test: nested session scope validation

### 2.2 - macOS Sandboxing

**Strategy:** Apple Containerization framework on macOS 26+ (Tahoe). Fallback to warning on older versions.

**Tasks:**
- [x] Research Apple Containerization framework API for Rust FFI
- [x] Implement macOS sandbox using Containerization (or sandbox-exec as interim)
- [x] Implement graceful fallback: warn user on unsupported macOS versions
- [x] Test: equivalent isolation to Linux implementation

---

## Phase 3: Git Integration (Detailed)

**Tasks:**
- [x] Create `tmax-git` crate using `git2`
- [x] Implement auto-detection: on session create, detect if cwd is in a git repo or worktree
- [x] Store detected git info in session metadata (branch, worktree path, repo root)
- [x] Implement `tmax new --worktree <branch>` shorthand (create worktree via git2, set as session cwd)
- [x] Implement `tmax worktree clean <session>` (remove worktree + kill session)
- [x] Expose git metadata via session info API (for pane border display)
- [x] Implement dirty/clean state detection for display

---

## Phase 4: Terminal Client (Detailed)

### 4.1 - Virtual Terminal Rendering

**Tasks:**
- [x] Create `tmax-client` crate using `crossterm`
- [x] Implement virtual terminal screen buffer (grid of cells with attributes)
- [x] Implement ANSI escape code parser (or use `vte` crate) for interpreting PTY output
- [x] Implement pane layout engine (horizontal/vertical splits, resize)
- [x] Implement pane border rendering with metadata (label, git info, sandbox state, [EDIT]/[VIEW])
- [x] Implement status bar with session info

### 4.2 - Input and Keybindings

**Tasks:**
- [x] Implement Ctrl+Space prefix key system
- [x] Implement all keybindings from brainstorm (split, navigate, search, markers, etc.)
- [x] Implement raw terminal input forwarding to PTY (when not in prefix mode)
- [x] Implement mouse support via crossterm events

### 4.3 - Scrollback UX

**Tasks:**
- [x] Implement scroll mode (enter/exit)
- [x] Implement search with regex highlight
- [x] Implement marker jump navigation
- [x] Implement smooth scrolling with mouse wheel

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

- [x] Create sessions that run arbitrary executables in PTYs
- [x] Stream PTY output to multiple concurrent subscribers in real time
- [x] Edit attachments can send input; view attachments cannot
- [x] Attachment lifecycle is unambiguous (`attachment_id` required for detach/input ownership)
- [x] Sessions can be nested with parent-child tracking
- [x] CLI can create, list, kill, attach, stream, and subscribe
- [x] WebSocket bridge streams raw PTY bytes to xterm.js
- [x] Filesystem sandboxing isolates session writes to declared paths
- [x] Nested sessions inherit and narrow sandbox scope
- [x] Git worktree auto-detection and display in session metadata
- [x] Terminal client renders split panes with borders and metadata

### Non-Functional Requirements

- [x] Session creation < 100ms
- [x] Output streaming latency < 10ms from PTY to subscriber
- [x] Memory per session < 5MB (excluding scrollback)
- [x] Single installation artifact per platform (may include multiple role binaries: server/cli/web/client)
- [x] Works on Linux (x86_64, aarch64) and macOS (aarch64)

Latest perf smoke measurement on native Ubuntu x86_64 reports RSS delta `4992KB` for one active session (`integration_perf_smoke_create_and_stream_latency`), meeting the 5MB target.

### Quality Gates

- [x] All phases have integration tests
- [x] Protocol types have serialization round-trip tests
- [x] Transport limits enforced (chunk/frame caps) with rejection tests
- [x] Local socket auth/permissions tests pass on Linux and macOS
- [x] Sandbox has escape-attempt tests (symlinks, path traversal)
- [x] CI runs on Linux and macOS

### Testing Strategy

- [x] **Golden VT tests:** Feed known escape sequences through VtState parser, assert screen buffer matches expected output. Ensures terminal emulation correctness.
- [x] **Load tests:** Many panes + slow clients. Verify backpressure kicks in, no unbounded memory growth, fair scheduling.
- [x] **Deterministic replay tests:** Capture output logs from real sessions, replay through VtState, assert deterministic screen state. Regression-proof.
- [x] **Snapshot+delta recovery tests:** Disconnect client, reconnect at various seq gaps, verify correct state restoration.
- [x] **Input ownership tests:** Multiple edit attachments on same session, verify input-lock semantics.
- [x] **Connection writer tests:** Many subscriptions/events on one client connection, verify single-writer path preserves JSON framing.

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
| Concurrent writers on one client socket | Framing corruption, dropped/garbled events | Enforce per-client single writer task + bounded outbound queue |
| Unix socket overexposed to local users | Unauthorized read/input injection | Socket mode 0600, runtime dir 0700, peer uid verification |
| JSON-lines payload overhead on PTY output | Miss latency targets under load | Enforce bounded chunk size, benchmark early, add binary fast-path if needed |
| Terminal UI complexity (Phase 4) | Delayed or poor quality | Phase it last; web bridge is primary UI; consider using `vte` crate for parsing |
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
