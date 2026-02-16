---
name: tmax
description: Use tmax to run commands in sandboxed terminal sessions, coordinate multi-agent work, and manage session lifecycle. Activate when the user asks to run commands via tmax, manage terminal sessions, coordinate agents, or when you need isolated sandboxed execution environments.
user-invocable: false
---

# Using tmax

tmax is a programmable terminal multiplexer. You interact with it through the `tmax` CLI over a Unix socket. The server must be running before you can use any commands.

## Install

If `tmax` is not installed, install it:

```bash
# macOS
brew install tmax

# From source (requires Rust 1.85+)
cargo install --git https://github.com/ardabotai/tmax tmax-cli tmax-local
```

This installs `tmax` (the CLI) and `tmax-local` (the server).

## Starting and stopping

```bash
tmax up                       # Start the server (background)
tmax up --foreground          # Start in foreground
tmax down                     # Stop the server
tmax health --json            # Check if server is running and healthy
```

The socket path is auto-discovered. Only pass `--socket <path>` if using a non-default location.

## Running tasks (primary workflow)

`run-task` is the command you should use most. It creates a session, streams output, waits for exit, and returns the result in one step.

```bash
# Run a command and get its output
tmax run-task 'npm test'

# With a timeout (cancels if exceeded)
tmax run-task --timeout-ms 30000 'cargo build'

# With retries for flaky commands
tmax run-task --retry-attempts 3 --retry-base-ms 500 'curl https://api.example.com'

# Suppress streaming output, just get the result
tmax run-task --no-stream --json 'echo done'
```

Exit behavior:
- Exit code 0 = success
- Non-zero exit code or signal = `run-task` exits with an error
- `--json` flag returns structured result with `session_id`, `exit_code`, `signal`

### Monitoring and cancelling tasks

```bash
tmax tail-task <session-id>                # Stream a running task until exit
tmax tail-task <session-id> --json         # Get structured result
tmax cancel-task <session-id>              # Cancel a task
tmax cancel-task <session-id> --cascade    # Cancel task and all child sessions
```

## Session management

For lower-level control when `run-task` isn't enough:

```bash
# Create a session
tmax new 'long-running-server'
# Returns JSON with session_id

# List all sessions
tmax list
tmax list --tree              # Show parent/child hierarchy

# Session details
tmax info <session-id>

# Kill a session
tmax kill <session-id>
tmax kill <session-id> --cascade   # Kill children too
```

## Sandboxed execution

Restrict filesystem access for a session. The sandbox denies all writes by default and only allows paths you explicitly permit.

```bash
# Only allow writes to /tmp/workdir
tmax run-task --sandbox-write /tmp/workdir 'echo safe > /tmp/workdir/out.txt'

# Multiple writable paths
tmax new --sandbox-write /tmp/a --sandbox-write /tmp/b 'echo hi'

# Disable sandbox (use with caution)
tmax new --no-sandbox 'echo unrestricted'
```

Child sessions inherit their parent's sandbox and can only narrow it further, never widen it:

```bash
tmax new --parent <parent-id> --sandbox-write /tmp/workdir/sub 'echo nested'
```

## Inter-agent messaging

Send messages between sessions. Useful when multiple agents need to coordinate.

```bash
# Send a message to another session
tmax msg send --to <session-id> "Task complete, results in /tmp/output"

# Check for messages
tmax msg list <your-session-id> --unread

# Acknowledge a message (marks it read)
tmax msg ack <your-session-id> <message-id>

# Count unread messages
tmax msg unread <your-session-id>
```

## Shared task lists

Coordinate work across agents with a shared task list. Tasks support dependencies, claims, and status tracking.

```bash
# Create a workflow first
tmax workflows create "my-workflow" --root <session-id>

# Add tasks to the workflow
tmax tasks add "Run unit tests" --workflow <workflow-id> --created-by <session-id>
tmax tasks add "Run integration tests" --workflow <workflow-id> --created-by <session-id> --depends-on <task-id>

# Claim and work on a task
tmax tasks claim <task-id> <session-id>
tmax tasks status <task-id> <session-id> in_progress
tmax tasks status <task-id> <session-id> done

# List tasks
tmax tasks list --workflow <workflow-id> --session <session-id>
tmax tasks list --workflow <workflow-id> --session <session-id> --include-done
```

Valid statuses: `todo`, `in_progress`, `blocked`, `done`

## Git worktrees

Create isolated git worktrees per session so agents can work on separate branches without conflicts:

```bash
# Create a session with its own worktree for a branch
tmax new --worktree feature-branch 'cargo test'

# Clean up worktree and session when done
tmax worktree clean <session-id>
```

## Common patterns

### Run a command and check the result

```bash
tmax run-task --json --timeout-ms 60000 'cargo test --workspace'
```

Parse the JSON output to check `exit_code`. Zero means success.

### Parallel agent execution

Create multiple sessions and tail them:

```bash
# Start parallel tasks
tmax new --label "tests" 'cargo test'
tmax new --label "lint" 'cargo clippy'

# Monitor them
tmax list
tmax tail-task <test-session-id>
tmax tail-task <lint-session-id>
```

### Sandboxed agent with messaging

```bash
# Create a sandboxed worker
tmax new --sandbox-write /tmp/work --label "worker-1" 'process-data /tmp/work'

# Send it instructions
tmax msg send --to <worker-session-id> "Process files in /tmp/work/batch-42"

# Check for its response
tmax msg list <your-session-id> --unread
```

## Important notes

- Always check `tmax health` before starting work to verify the server is running
- `run-task` is preferred over manual `new` + `attach` + `subscribe` for most use cases
- Session IDs are returned as JSON when you create sessions — parse and store them
- All commands return JSON by default for programmatic consumption
- The `--cascade` flag on `kill` and `cancel-task` recursively destroys child sessions
- Sandbox violations are silent failures (writes outside allowed paths are blocked, not errored)
