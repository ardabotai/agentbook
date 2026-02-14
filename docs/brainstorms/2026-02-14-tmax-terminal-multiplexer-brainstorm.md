# tmax - Programmable Terminal Multiplexer for AI Workflows

**Date:** 2026-02-14
**Status:** Brainstorm

## What We're Building

A ground-up terminal multiplexer built in Rust, designed as a **tool** that AI agents and orchestration systems consume. tmax provides sandboxed, streamable terminal sessions with first-class git worktree integration. It is agent-agnostic - it doesn't embed or know about AI agents. Instead, it exposes a clean API that any program (orchestrator, agent, CLI, web app) can use to create sessions, stream output, and send input.

**Primary use case:** An OpenClaw-style orchestrator agent runs inside a tmax session (unsandboxed, no worktree). Humans interact with the orchestrator through the Next.js web GUI - assigning tasks, reviewing plans, and watching it work. The orchestrator creates sandboxed sub-agent sessions (1:1 per agent, each scoped to a git worktree) and monitors their output via event stream subscriptions. The human sees everything - the orchestrator session and all sub-agent sessions - in the same web GUI, with the ability to scroll, search, and send input to any session.

### Core Value Proposition

- **Programmable API** via Unix socket + JSON-lines - any process can create/control/stream sessions
- **Worktree-scoped sandboxing** - sessions can be filesystem-isolated to their git worktree (hard OS-native enforcement)
- **First-class git worktree integration** where each worktree maps to a session
- **Output streaming** for web clients via a bridge service (tmax-web)
- **Native scrolling, search, markers, and diff views** for human review
- **Simple keybindings** with Ctrl+Space prefix for terminal client
- **Single binary installation** via Rust, runs on any system
- **Library-first architecture** (`libtmax`) enabling terminal UI, web bridge, and programmatic embedding

## Why This Approach

### Library-first architecture

The core logic lives in `libtmax`, and both the terminal UI and web bridge are clients. This is essential because tmax has two equally important interfaces:

1. **Terminal client** (`tmax-client`) - for developers who want a native terminal experience
2. **Web bridge** (`tmax-web`) - WebSocket bridge for the Next.js GUI

The library boundary ensures both clients get the same capabilities and neither is second-class.

### tmax as a tool, not a platform

tmax deliberately does NOT embed AI agent logic, orchestration, or task management. It provides:
- Sandboxed terminal sessions
- Output streaming
- Git worktree lifecycle management
- Input forwarding

The orchestrator, agents, task assignment, and web GUI are all external consumers. This keeps tmax focused, testable, and reusable across different AI workflows.

### Why not wrap tmux?

Full control over PTY management, scrollback, and the streaming API. tmux's protocol wasn't designed for structured programmatic communication or real-time web streaming, and bolting it on creates a leaky abstraction.

### Why not Zellij?

Zellij is excellent but optimized for human-interactive use. Its WASM plugin system adds memory overhead (~80MB baseline vs tmux's ~6MB). tmax targets the lean-and-fast end with a programmatic API that Zellij doesn't offer.

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Language | Rust | Single binary, max performance, strong terminal ecosystem |
| Architecture | Library-first (`libtmax` + clients) | Enables terminal UI, web bridge, and programmatic embedding equally |
| Relation to tmux | Ground-up replacement | Full control over PTY, scrollback, streaming API |
| Control API | Unix socket + JSON-lines | Lightweight, debuggable, zero deps, streaming support |
| Web transport | Bridge service (`tmax-web`) | Translates Unix socket to WebSocket. Keeps web concerns out of core. |
| Web rendering | xterm.js (same as terminal) | Stream raw PTY bytes to xterm.js in browser. Single rendering path, identical output everywhere. |
| Attach modes | Edit and View | Edit: full control. View: read-only (scroll, search, markers). Per-attachment, not per-session. |
| Web GUI model | 1 edit + N view attachments | Orchestrator is the only edit attachment in the GUI. All sub-agent attachments are view-only. Humans command sub-agents through the orchestrator. |
| Keybindings | Ctrl+Space prefix | Easy to type, rarely conflicts, familiar prefix model |
| Git worktrees | Auto-detect, not a constraint | If launched in a git repo, optionally auto-create worktree. Pane border shows worktree/branch info. No structural coupling. |
| Human review UX | Scroll + search + markers + diff | Markers for navigation, built-in diff viewer |
| Sandbox scope | Arbitrary paths + shared dirs | Sandbox to any directory. Declare additional shared writable paths (cache, /tmp). Independent of worktree. |
| Sandbox enforcement | Hard per-platform, configurable per-session | OS-native isolation (Linux namespaces, macOS sandbox). Default on, opt-out available. |
| Session model | Composable, nestable | Sessions are independent primitives. No forced 1:1 with agents or worktrees. Any process can create any number of sessions. |
| Session payload | Agent harness executable, not shell | PTY spawns the agent executable directly. Shell is opt-in, not default. |
| Output consumption | Event stream subscription | Multiple subscribers (orchestrator + web bridge + terminal) can stream the same session concurrently |
| Agent awareness | None - tmax is a tool | tmax doesn't know about agents. Orchestrators and agents are external consumers. |
| Orchestrator pattern | OpenClaw-style | Root agent creates sessions, spawns sub-agents, monitors output via event streams |
| MVP scope | Phased (see Delivery Phases below) | Core + CLI + API first, web bridge second, sandboxing third, terminal UI last |

## Feature Breakdown

### Core Multiplexer (libtmax)
- PTY management via `portable-pty`
- **Arbitrary process execution** - sessions launch a specified executable, NOT a shell by default. The typical payload is an agent harness (e.g., `claude-code`, custom agent runner, OpenClaw worker), not bash/zsh. Shell sessions are supported but are not the primary use case.
  - `tmax new --exec "claude-code --task 'fix auth bug'" --worktree feat-auth --sandbox`
  - `tmax new --shell` (opt-in for traditional shell session)
- Sessions, windows, panes (horizontal/vertical splits)
- mmap'd scrollback for unlimited persistent history
- Client-server architecture over Unix domain sockets
- Session persistence (detach/reattach)
- Session metadata (labels, tags, sandbox state, worktree path, executable)

### Control API (Unix Socket + JSON-lines)
- **Session management:** create, destroy, list, attach (edit or view mode), detach
- **Session model:** 1:1 mapping - each session runs one process (one sub-agent)
- **Pane management:** split (h/v), resize, close, focus
- **I/O:** send keys/input, read output (polling or streaming)
- **Markers:** insert named markers into scrollback, list markers, jump to marker
- **Diff views:** push file diffs that render inline in output
- **Event streaming:** subscribe to real-time events per session:
  - Output chunks (new terminal output as it arrives)
  - Session lifecycle (created, started, exited, destroyed)
  - Exit codes and signals
  - Marker insertions
  - **Multiple concurrent subscribers** - orchestrator and web bridge can both stream the same session simultaneously
- **Metadata:** label sessions, set tags, query state
- All commands return structured JSON responses

### Git Worktree Integration (Auto-detect, Not a Constraint)
- **Auto-detection:** When a session launches in a git repo or worktree, tmax detects it automatically
- **Pane border display:** Shows branch name, worktree path, and dirty/clean state in the pane border
- **Convenience command:** `tmax new --worktree <branch>` creates a worktree + session in one command (shorthand, not a structural requirement)
- **No coupling:** Sessions are NOT structurally tied to worktrees. You can have:
  - Multiple sessions in the same worktree (one runs tests, one writes code)
  - Sessions with no worktree at all (database, dev server, orchestrator)
  - Sessions that outlive their worktree
- Quick-switch between sessions that happen to be in worktrees
- Cleanup command to remove worktree + session together (opt-in convenience)

### Filesystem Sandboxing (Independent of Worktree)
- Sessions can be sandboxed to restrict writes to specified directories
- **Sandbox scope is arbitrary** - not tied to worktrees. Configure any path(s):
  - `tmax new --sandbox-write /repo/worktrees/feat-auth --exec "agent-runner"`
  - `tmax new --sandbox-write /repo/worktrees/feat-auth --sandbox-write /shared/cache --exec "agent-runner"`
- **Shared writable paths:** Sessions can declare additional writable directories beyond the primary scope (build caches, `/tmp`, shared output dirs)
- Everything outside declared writable paths is accessible as read-only
- **Hard enforcement** using OS-native sandboxing:
  - **Linux:** Filesystem namespaces (`unshare`/`mount` namespaces) - bind-mount writable dirs, everything else read-only
  - **macOS:** `sandbox-exec` profiles restricting write paths
- **Configurable per-session:** `--sandbox` (default) or `--no-sandbox`
- Pane border indicates sandbox state and scope path(s)

### Session Nesting
- Sessions can spawn child sessions, forming a tree hierarchy
- **Sandbox inheritance rule:** A sandboxed parent can only create child sessions with writable paths that are subsets of (or equal to) the parent's writable paths. Scope narrows or stays equal, never widens.
  - Example: Parent with write access to `/repo/feat-auth/` + `/shared/cache/` can spawn a child with `/repo/feat-auth/frontend/` + `/shared/cache/` but NOT `/repo/fix-bug/`
  - Unsandboxed parents can create children with any scope
- **Parent-child relationship tracking:** `tmax list --tree` shows the session hierarchy
- **Lifecycle propagation:** Killing a parent session optionally kills all children (configurable: cascade vs orphan)
- **Composable:** No limit on sessions per worktree, agents per session, or nesting depth. Sessions are independent primitives that compose freely within sandbox rules.

### Web Bridge (tmax-web)
- Separate Rust service connecting to tmax-server via Unix socket
- **Streams raw PTY bytes over WebSocket** to xterm.js in the browser - identical rendering to the native terminal client. No separate web rendering layer needed.
- Forwards input from web clients back to session PTYs
- Provides REST endpoints for session listing, metadata, worktree status, session tree
- Handles multiple concurrent web clients
- The Next.js app embeds one xterm.js instance per session pane, each connected to its own WebSocket stream

### Human Review UX
- Native smooth scrolling (no copy-mode gymnastics)
- `/search` with regex support and highlight
- Named markers: any process can insert markers that humans jump between
- Built-in diff view: processes can push file diffs that render inline
- Full mouse support (click to focus, drag to resize, scroll wheel)

### Keybindings (Terminal Client - Ctrl+Space prefix)
- `Ctrl+Space, c` - new window
- `Ctrl+Space, n/p` - next/previous window
- `Ctrl+Space, 1-9` - switch to window N
- `Ctrl+Space, |` - vertical split
- `Ctrl+Space, -` - horizontal split
- `Ctrl+Space, h/j/k/l` - navigate panes (vim-style)
- `Ctrl+Space, d` - detach
- `Ctrl+Space, /` - search scrollback
- `Ctrl+Space, m` - list markers
- `Ctrl+Space, w` - list worktree sessions
- `Ctrl+Space, ?` - help overlay

### Attach Modes (Edit vs View)
Two modes for attaching to a session, enforced per-attachment (not per-session - the same session can have one edit attachment and multiple view attachments simultaneously):

**Both modes stream PTY output in real time** - there is no difference in output delivery. Edit and view attachments receive the same live stream with zero lag.

**Edit attachment** (`tmax attach --edit <session>`):
- **Real-time output streaming**
- Send input/keystrokes to the PTY
- Use tmax keybindings (Ctrl+Space prefix)
- Session management (split, close, resize, kill)
- Scroll, search, jump to markers
- Pane border indicator: `[EDIT]`

**View attachment** (`tmax attach --view <session>`):
- **Real-time output streaming** (identical to edit)
- Scroll through output (native scrolling, mouse wheel)
- Search scrollback (`/` search)
- Jump to markers
- **No input** - keystrokes are not forwarded to the PTY
- **No session control** - cannot split, close, resize, or kill
- **No tmax keybindings** except scroll/search/markers
- Pane border indicator: `[VIEW]`

**Key properties:**
- Mode is per-attachment, not per-session. Multiple viewers + one editor on the same session is valid.
- API clients specify mode on attach: `{"cmd": "attach", "session": "s1", "mode": "view"}`

**Web/Mobile GUI model:**
- **One edit attachment:** The orchestrator session is the only one mounted with an edit attachment in the GUI. Humans type commands to the orchestrator here.
- **All other attachments are view-only:** Sub-agent sessions are attached as view-only. Humans watch agent output but cannot send input directly.
- **Humans control sub-agents indirectly:** To stop, restart, or redirect a sub-agent, the human tells the orchestrator via the edit attachment. The orchestrator manages all downstream sessions through the tmax API (not through edit attachments to them).
- This enforces a clean command chain: Human -> Orchestrator (edit attachment) -> tmax API -> Sub-agent sessions. No bypassing the orchestrator.

**Terminal client:** More permissive - developers can create edit attachments to any session for debugging/intervention. The terminal client is the escape hatch when direct access is needed.

### Unified Rendering (Terminal + Web)
- **Single rendering path:** tmax renders pane borders, status bar, git info, and sandbox indicators into the terminal stream using ANSI escape codes
- **Terminal client:** native terminal renders the stream directly
- **Web client:** xterm.js renders the same stream identically in the browser
- `crossterm` for terminal output generation (cross-platform)
- True color and Unicode support
- Configurable pane borders showing: git branch, worktree path, sandbox scope, session label
- Full mouse support (click to focus, drag to resize, scroll wheel) - works in both xterm.js and native terminal

## Technical Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        tmax-server (libtmax core)                │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │ Session 0: Orchestrator                                    │  │
│  │ sandbox: OFF | no worktree | label: "orchestrator"         │  │
│  │ PTY ──> mmap                                               │  │
│  │                                                            │  │
│  │ Runs the root agent (OpenClaw-style). Human sends it       │  │
│  │ tasks via web GUI input. It creates sub-agent sessions     │  │
│  │ and subscribes to their event streams to coordinate work.  │  │
│  └────────────────────────────────────────────────────────────┘  │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ Session 1    │  │ Session 2    │  │ Session 3    │  ...     │
│  │ wt: feat-auth│  │ wt: fix-bug  │  │ wt: refactor │          │
│  │ sandbox: ON  │  │ sandbox: ON  │  │ sandbox: ON  │          │
│  │ PTY ──> mmap │  │ PTY ──> mmap │  │ PTY ──> mmap │          │
│  │ (sub-agent)  │  │ (sub-agent)  │  │ (sub-agent)  │          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
│                                                                  │
│  All sessions support multiple concurrent event stream           │
│  subscribers (orchestrator + web bridge + terminal client)       │
└──────────┬──────────────────────────────┬────────────────────────┘
           │                              │
      Unix Socket                    Unix Socket
           │                              │
   ┌───────┴───────┐             ┌────────┴────────┐
   │ tmax-client   │             │   tmax-web      │
   │ (terminal UI) │             │   (bridge)      │
   └───────────────┘             └────────┬────────┘
                                          │ WebSocket
                                   ┌──────┴──────────────┐
                                   │    Next.js GUI      │
                                   │                      │
                                   │ ┌──────────────────┐ │
                                   │ │ Orchestrator     │ │
                                   │ │ [EDIT] xterm.js  │ │
                                   │ │ human types here │ │
                                   │ ├──────────────────┤ │
                                   │ │ Agent 1 │Agent 2 │ │
                                   │ │ [VIEW]  │[VIEW]  │ │
                                   │ │ scroll/ │scroll/ │ │
                                   │ │ search  │search  │ │
                                   │ └──────────────────┘ │
                                   └──────────────────────┘

Human talks to orchestrator via web GUI.
Orchestrator spawns sub-agent sessions and monitors them.
Human sees orchestrator + all sub-agents in the same GUI.
All sessions streamable to any number of concurrent consumers.
```

### Rust Workspace Structure

```
tmax/
  Cargo.toml              # workspace root
  crates/
    libtmax/              # core library - PTY, sessions, panes, scrollback
    tmax-server/          # server binary - manages sessions, listens on socket
    tmax-client/          # terminal UI client
    tmax-cli/             # CLI tool (tmax new-session, tmax send-keys, etc.)
    tmax-protocol/        # shared protocol types (JSON-lines messages)
    tmax-web/             # WebSocket bridge service for web clients
    tmax-git/             # git worktree integration
    tmax-sandbox/         # OS-native filesystem sandboxing
```

### Key Rust Crates

| Crate | Purpose |
|-------|---------|
| `portable-pty` | Cross-platform PTY management |
| `crossterm` | Terminal I/O and rendering |
| `serde` + `serde_json` | JSON serialization for protocol |
| `tokio` | Async runtime for server and web bridge |
| `tokio-tungstenite` | WebSocket support for tmax-web |
| `axum` | HTTP framework for tmax-web REST endpoints |
| `git2` | Git operations (worktree management) |
| `memmap2` | Memory-mapped files for scrollback |
| `regex` | Scrollback search |
| `nix` | Linux namespace/sandbox syscalls |

**Web (Next.js GUI):**

| Package | Purpose |
|---------|---------|
| `@xterm/xterm` 6.0 | Terminal emulator in browser - renders raw PTY stream. Scoped package (old `xterm` deprecated). |
| `@xterm/addon-fit` | Auto-resize xterm.js to container |
| `@xterm/addon-web-links` | Clickable links in terminal output |

**Multi-session rendering strategy:**
- One xterm instance per **visible** pane only (not all sessions)
- Offscreen sessions: buffer chunks in JS, no xterm rendering
- On pane switch: create xterm, replay buffered chunks, then stream live
- Controls CPU/memory when monitoring 10+ agent sessions

## Resolved Questions

1. **Config format:** TOML - Rust ecosystem standard, simple, well-supported.
2. **Remote sessions:** Local-only for v0.1. SSH forwarding deferred to v0.2.
3. **Scrollback storage:** Dual-layer. In-memory circular buffer (VecDeque) for live streaming with global sequence IDs and per-client cursors. Optional append-only disk log for long history/audit/persistence. mmap is NOT on the live hot path.
4. **Mouse support:** Full mouse support from day one - click to focus panes, drag to resize, scroll wheel for scrollback.
5. **Primary UI:** Both terminal client and Next.js web GUI are equally important.
6. **Web transport:** Bridge service (tmax-web) translates Unix socket to WebSocket. Keeps web concerns out of core.
7. **Web interactivity:** View + interact. Humans can view output AND send input through the web GUI.
8. **Agent integration:** None. tmax is a tool. Orchestrators and agents are external consumers of the API.

## Delivery Phases

### Phase 0: Core Engine (libtmax + server + CLI + protocol)
The foundation. No UI - just the execution layer.
- `libtmax`: PTY management, sessions, mmap'd scrollback, session nesting, metadata
- `tmax-server`: daemon managing sessions, Unix socket listener
- `tmax-cli`: create, list, kill sessions, send input, stream output
- `tmax-protocol`: JSON-lines message types
- Edit/view attachment modes via API
- Multi-subscriber event streaming
- **Validates:** Can the orchestrator create sessions, spawn agent harnesses, and stream output?

### Phase 1: Web Bridge (tmax-web)
Humans can watch and interact.
- `tmax-web`: WebSocket bridge streaming raw PTY bytes
- REST endpoints for session listing, metadata, session tree
- xterm.js integration proof-of-concept
- Edit/view attachment support over WebSocket
- **Validates:** Can humans watch agents work in real time via xterm.js?

### Phase 2: Sandboxing (tmax-sandbox)
The key differentiator.
- Linux implementation first (filesystem namespaces)
- macOS implementation second (research alternatives to deprecated sandbox-exec)
- Sandbox scope configuration (arbitrary paths + shared writable dirs)
- Nesting scope enforcement (narrows, never widens)
- **Validates:** Can sessions be truly isolated to their declared writable paths?

### Phase 3: Git Integration (tmax-git)
Convenience layer, not structural.
- Auto-detect worktree/branch on session launch
- Pane border git info display
- `tmax new --worktree <branch>` shorthand
- Worktree + session cleanup command
- **Validates:** Does git-aware session creation feel seamless?

### Phase 4: Terminal Client (tmax-client)
The hardest piece - full terminal UI.
- Virtual terminal rendering (pane borders, splits, status bar)
- Ctrl+Space keybinding system
- Mouse support (click, drag, scroll)
- Scroll/search/markers in terminal
- Pane border indicators (git, sandbox, edit/view, labels)
- **Validates:** Is the native terminal experience competitive with tmux/zellij?

## Out of Scope (all phases)

- Next.js GUI itself (tmax provides the web bridge; the GUI is a separate project)
- WASM plugin system
- Collaborative/multiplayer sessions
- Built-in file editor
- Remote session management (SSH forwarding)
- Agent orchestration or task management

## Design Risks

1. **macOS sandboxing:** `sandbox-exec` is deprecated. Need to research alternatives (Seatbelt profiles, endpoint security framework, or container-based approaches). Linux implementation is straightforward.
2. **Terminal UI complexity:** Virtual terminal rendering with splits, resize, cursor management is the hardest part of any terminal multiplexer. Phasing it last de-risks the overall project.
3. **Competing with tmux/zellij base quality:** The terminal client will be compared against mature tools. The web bridge + agent API are the real differentiators - the terminal client needs to be good enough, not best-in-class.
