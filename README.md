# tmax

**A terminal multiplexer that actually likes robots.**

tmux was built for humans. tmax was built for the agents that replaced them. (Just kidding. Mostly.)

tmax is a programmable terminal multiplexer with a JSON-lines API over Unix sockets. It gives AI agents a proper execution environment — sandboxed terminals, real-time output streaming, inter-agent messaging, and shared task coordination — while letting you watch everything from a browser or native TUI.

## Install

### macOS (Homebrew)

```bash
brew install tmax
```

### Linux / macOS (cargo)

Requires [Rust](https://rustup.rs/) 1.85+.

```bash
cargo install --git https://github.com/ardabotai/tmax tmax-cli tmax-local
```

This installs two binaries: `tmax` (the CLI) and `tmax-local` (the server). That's all you need for local use.

### Pre-built binaries

Grab a tarball from [GitHub Releases](https://github.com/ardabotai/tmax/releases), extract it, and add the `bin/` directory to your `PATH`:

```bash
tar -xzf tmax-aarch64-apple-darwin.tar.gz
export PATH="$PWD/tmax-aarch64-apple-darwin/bin:$PATH"
```

### Verify

```bash
tmax up && tmax health --json && tmax down
```

---

## What does using tmax look like?

Install the [agent skill](#agent-skill), start tmax, and talk to your agent normally. The agent uses tmax behind the scenes to run commands in isolated sessions instead of raw shell exec.

**You:** "Run the test suite, fix any failures, and make sure linting passes too."

Your agent (with the tmax skill installed) will:

```bash
# Start tmax if it isn't running
tmax up

# Run tests in a sandboxed session with a timeout
tmax run-task --timeout-ms 120000 --sandbox-write /home/user/project \
  'cd /home/user/project && npm test'

# Tests fail — agent reads the output, fixes the code, runs again
tmax run-task --timeout-ms 120000 --sandbox-write /home/user/project \
  'cd /home/user/project && npm test'

# Run linting in parallel
tmax run-task --timeout-ms 60000 'cd /home/user/project && npm run lint'
```

Each command runs in its own PTY session with real terminal output (colors, progress bars, interactive prompts all work). The agent sees the full output, not just exit codes. And if you're curious what it's doing, you can watch live:

```bash
tmax list                            # See what sessions are running
tmax attach <session-id> --view      # Watch an agent work in real time
```

### Multi-agent coordination

For bigger tasks, agents can split work across sandboxed sessions and coordinate through tmax's built-in messaging and task lists:

**You:** "Refactor the auth module. Run tests on a separate branch so you don't break main."

```bash
# Agent creates an isolated git worktree
tmax new --worktree refactor-auth --sandbox-write /home/user/project \
  --label "refactor" 'cd /home/user/project && edit-files...'

# Spawns a second session to run tests while it works
tmax new --worktree refactor-auth --label "tests" \
  'cd /home/user/project && cargo test --workspace'

# Agents message each other
tmax msg send --to <test-session> "Refactoring complete, re-run tests"
tmax msg list <refactor-session> --unread
```

You see all of this in `tmax list --tree`, or watch any session live with `tmax attach`.

---

## Quick start

```bash
# Start tmax
tmax up

# Create a session
tmax new 'echo hello from the future'

# See what's running
tmax list

# Check health
tmax health --json

# Stop tmax
tmax down
```

The socket path is auto-discovered (`$XDG_RUNTIME_DIR/tmax/tmax.sock` or `/tmp/tmax-$UID/tmax.sock`). Pass `--socket <path>` only if you enjoy typing.

---

## Agent skill

The repo includes a drop-in [Agent Skills](https://agentskills.io) standard skill at `skills/tmax/SKILL.md`. Install it and your agent knows how to use tmax — start the server, run sandboxed tasks, coordinate with other agents, manage session lifecycle. No tutorial needed.

```bash
# Install for Claude Code
cp -r skills/tmax ~/.claude/skills/tmax
```

Works with Claude Code, ChatGPT/Codex, Cursor, GitHub Copilot, and any tool that supports the [Agent Skills](https://agentskills.io) open standard.

### What the skill teaches the agent

The skill gives the agent practical knowledge of:

- **`run-task`** — the primary command: run a command, stream output, wait for exit, handle retries and timeouts
- **Sandboxing** — restrict filesystem access per session, nest sandboxes for sub-agents
- **Messaging** — send messages between sessions for multi-agent coordination
- **Shared tasks** — create task lists with dependencies, claims, and status tracking
- **Git worktrees** — create isolated branches per session
- **Session lifecycle** — create, list, monitor, kill sessions

The agent doesn't need to understand PTY lifecycle, attachment IDs, or the JSON-lines protocol. The skill abstracts all of that away.

### Rust SDK

For agents written in Rust, `tmax-agent-sdk` provides a high-level async client:

```rust
let mut client = AgentClient::connect(&socket_path).await?;
let result = client.run_task(options, |output| { /* stream handler */ }).await?;
```

---

## What you can do as a human

While agents do the heavy lifting, you manage the server, watch what's happening, and jump in when needed.

### Watch agent sessions

```bash
tmax list                            # See all running sessions
tmax list --tree                     # See the parent/child hierarchy
tmax info <session>                  # Session details (exit code, git branch, sandbox, etc.)
```

### Attach to a session

Connect to a live session to see its terminal output. View mode is read-only (watch but don't type). Edit mode lets you interact.

```bash
tmax attach <session> --view         # Watch an agent work (read-only)
tmax attach <session>                # Jump in and type (edit mode)
tmax detach <attachment>             # Disconnect
```

### Native terminal UI

`tmax-client` is a full terminal UI similar to tmux — pane splits, borders showing git branch and sandbox status, keybindings, scrollback search, and marker navigation. The "control room" view.

Prefix key is `Ctrl+Space`, then:
- `|` / `-` — split horizontal / vertical
- `h/j/k/l` — navigate panes
- `/` — search scrollback
- `m` — jump to marker

### WebSocket bridge

`tmax-web` streams sessions to a browser over WebSocket. Point an xterm.js terminal at it for a web dashboard showing what your agents are up to.

```
GET  /api/sessions              # List sessions (JSON)
GET  /api/sessions/:id          # Session details
WS   /ws/session/:id?mode=view  # Live terminal stream
```

### Start and stop the server

```bash
tmax up                    # Start tmax-local (default, background)
tmax up --foreground       # Stay in foreground (see logs)
tmax down                  # Stop gracefully
tmax health --json         # Check if everything is OK
```

---

## Key features

### Sandboxing

Every session can be sandboxed with a deny-default filesystem policy. Because trusting an AI agent with unrestricted filesystem access is the "hold my beer" of software engineering.

- **macOS**: `sandbox-exec` with `(deny default)` base + explicit read/write allowlists
- **Linux**: user namespaces with read-only root remount + bind-mounted writable paths

Nested sessions inherit and narrow their parent's sandbox. A child can't escape to paths the parent didn't allow.

```bash
tmax new --sandbox-write /tmp/workdir 'echo safe'
tmax new --no-sandbox 'echo yolo'
```

### Communication policies

Control which agents can talk to each other:

- `--comms-policy open` (default) — any session can message any other
- `--comms-policy same_subtree` — sessions must share the same root ancestor
- `--comms-policy parent_only` — only direct parent/child messaging allowed

### Git integration

Auto-detect repos and create isolated worktrees per session:

```bash
tmax new --worktree feature-branch 'cargo test'
tmax worktree clean <session>    # Kill session + remove the worktree
```

---

## Mesh networking (optional)

Most people don't need this. Mesh networking is for when your agents live on different machines and need to talk to each other over the internet.

A node (`tmax-node`) extends the local server with: persistent cryptographic identity, an encrypted inbox, a friends list with trust tiers, and relay transport for NAT traversal. Think of it as "what if your terminal multiplexer had a social network, but a useful one."

```bash
tmax up --node --relay-host relay.example.com

tmax invite create --relay relay.example.com --scope dm_text --scope task_update
# On the other machine:
tmax invite accept <token>

tmax remote send <node-id> "hello from across the internet"
```

### Trust tiers

Not all friends are created equal:

| Tier | Allowed Messages |
|---|---|
| Public | Broadcast |
| Follower (default) | DM, Broadcast |
| Trusted | DM, Broadcast, TaskUpdate |
| Operator | DM, Broadcast, TaskUpdate, Command |

```bash
tmax friends trust <node-id> operator    # Full trust
tmax friends block <node-id>             # Nope
```

---

## CLI reference

### Server lifecycle

```
tmax up [--node] [--foreground]       Start tmax
tmax down                             Stop tmax
tmax health [--json]                  Health check
tmax server start|stop|status         Server management (legacy aliases)
```

### Sessions

```
tmax new [command]                    Create a session
tmax list [--tree]                    List sessions
tmax info <session>                   Session details
tmax kill <session> [--cascade]       Kill a session
```

### Attachments

```
tmax attach <session> [--view]        Attach to a session
tmax detach <attachment>              Detach
tmax send <session> <input>           Send input to a session
```

### Task flows

```
tmax run-task [command]               Create + stream + wait (the main one)
tmax tail-task <session>              Stream a running task
tmax cancel-task <session>            Cancel a task
```

### Messaging and coordination

```
tmax msg send --to <session> <body>   Send a message
tmax msg list <session> [--unread]    List messages
tmax tasks add <title> ...            Create a shared task
tmax tasks claim <task-id> <session>  Claim a task
tmax tasks status <task-id> ...       Update task status
```

### Mesh networking

```
tmax node info                        Show node identity
tmax invite create [--relay ...]      Create an invite token
tmax invite accept <token>            Accept an invite
tmax friends list|block|trust|remove  Manage friends
tmax remote send <node-id> <body>     Send a cross-machine message
tmax inbox list [--unread]            Read inbox
```

---

## Architecture

Two layers, because not everyone needs a mesh network:

- **Core** — terminal multiplexer, session engine, sandboxing, agent task flows, messaging. No network dependencies. This is all you need for local agent workflows.
- **Mesh networking** (optional) — node-to-node gRPC with end-to-end encryption, invite-based discovery, trust tiers, and relay/rendezvous NAT traversal.

<details>
<summary>Crate map</summary>

### Core

| Crate | What it does |
|---|---|
| `tmax-protocol` | Shared protocol types, constants, transport limits |
| `libtmax` | PTY/session engine, EventBroker, VT state, inbox/task state |
| `tmax-crypto` | ECDH, ECDSA, ChaCha20-Poly1305, key derivation |
| `tmax-sandbox` | Sandbox scope normalization + enforcement (macOS sandbox-exec, Linux namespaces) |
| `tmax-sandbox-runner` | Linux-only setuid helper for namespace sandboxing |
| `tmax-git` | Git repo/worktree metadata detection |

### Server + Clients

| Crate | What it does |
|---|---|
| `tmax-local` | Unix socket daemon: auth, session lifecycle, event fanout, comms policy |
| `tmax-cli` | CLI client: session management, agent task flows, messaging, shared tasks |
| `tmax-web` | HTTP/WebSocket bridge: REST + WS streaming, CORS, backpressure |
| `tmax-client` | Native terminal UI: pane splits, VT rendering, keybindings, scroll/search |
| `tmax-agent-sdk` | High-level async Rust client for agent workflows |

### Mesh networking (optional)

| Crate | What it does |
|---|---|
| `tmax-mesh` | Mesh primitives: node identity, friends, invite, inbox, crypto, transport |
| `tmax-mesh-proto` | Protobuf definitions for PeerService + HostService |
| `tmax-mesh-tests` | E2E integration tests for multi-node mesh scenarios |
| `tmax-node` | Unix socket + peer gRPC daemon: session management + mesh networking |
| `tmax-host` | Relay/rendezvous host: message forwarding + endpoint lookup |

</details>

## Development

```bash
cargo check                          # Type-check
cargo test --workspace               # Run all tests
cargo fmt --check                    # Check formatting
cargo clippy --workspace --all-targets -- -D warnings  # Lint
```

For local development, use `cargo run -p` instead of installed binaries:

```bash
cargo run -p tmax-local              # Start server
cargo run -p tmax-cli -- list        # CLI commands
```

## Production deployment

### Build a release

```bash
./scripts/package-release.sh
# Output: dist/tmax-x86_64-unknown-linux-gnu.tar.gz (or your platform)
```

The tarball contains all binaries (`tmax`, `tmax-local`, `tmax-web`, `tmax-client`, `tmax-sandbox-runner`), systemd service files, and config templates.

### Deploy to a Linux server

```bash
sudo ./scripts/deploy-linux.sh \
  --artifact dist/tmax-x86_64-unknown-linux-gnu.tar.gz
```

This extracts the release to `/opt/tmax/releases/`, symlinks it as `/opt/tmax/current`, installs the systemd service, restarts it, and runs a health check. If the health check fails, the deploy fails.

### Roll back

```bash
sudo ./scripts/rollback-linux.sh
```

Switches the symlink back to the previous release and restarts the service. Both scripts support `--dry-run` to preview what they'd do.

### Run as a systemd service

The deploy script handles this automatically. For manual setup, see the [step-by-step systemd guide](docs/operations/systemd.md).

## License

MIT
