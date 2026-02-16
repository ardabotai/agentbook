# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is tmax?

A programmable terminal multiplexer for AI workflows. Core transport is JSON-lines over Unix sockets. It enables agents to spawn, manage, and communicate across terminal sessions with features like sandboxing, inter-agent messaging, shared task lists, and encrypted mailbox communication.

## Build & Test Commands

```bash
cargo check                          # Type-check the full workspace
cargo test --workspace               # Run all tests
cargo test -p libtmax                # Test a single crate
cargo test -p libtmax test_name      # Run a single test
cargo fmt --check                    # Check formatting
cargo clippy --workspace --all-targets -- -D warnings  # Lint (CI enforces -D warnings)
```

## Smoke Testing

```bash
# Terminal 1: start server (auto-discovers socket path)
cargo run -p tmax-local

# Terminal 2: exercise CLI
cargo run -p tmax-cli -- new 'echo hello'
cargo run -p tmax-cli -- list
cargo run -p tmax-cli -- health --json
cargo run -p tmax-cli -- run-task --timeout-ms 10000 'echo task'
cargo run -p tmax-cli -- down
```

> `--socket` is only needed for custom socket paths. Both server and CLI auto-discover `$XDG_RUNTIME_DIR/tmax/tmax.sock` or `/tmp/tmax-$UID/tmax.sock`.

## Architecture

**Rust workspace** (`Cargo.toml` at root) using edition 2024, resolver v2. All crates live under `crates/`.

### Crate dependency flow (bottom-up)

```
tmax-crypto            ← cryptographic primitives: ECDH, ECDSA, ChaCha20-Poly1305, key derivation
tmax-protocol          ← shared types, constants, transport limits
    ↑
libtmax                ← PTY/session engine, EventBroker, VT state, inbox/task state (depends on tmax-crypto)
    ↑
tmax-sandbox           ← sandbox scope normalization + enforcement (sandbox-exec on macOS, namespaces on Linux)
tmax-sandbox-runner    ← Linux-only setuid helper binary for namespace sandboxing
tmax-git               ← git repo/worktree metadata detection via git2
    ↑
tmax-local            ← Unix socket daemon: auth, session lifecycle, event fanout, comms policy
    ↑
tmax-cli               ← CLI client (clap): session mgmt, agent task flows, messaging, shared tasks
tmax-web               ← HTTP/WebSocket bridge (axum): REST + WS streaming, CORS, backpressure
tmax-client            ← Native terminal UI: pane splits, VT rendering, keybindings, scroll/search
tmax-agent-sdk         ← High-level async client for agent workflows (execute_task, retry, health)
tmax-mesh              ← mesh networking primitives: node identity, friends, invite, inbox, transport (depends on tmax-crypto)
tmax-mesh-proto        ← protobuf definitions for PeerService + HostService
tmax-node              ← Unix socket + peer gRPC daemon: session mgmt + mesh networking (relay, peer, invite, inbox)
tmax-node-proto        ← protobuf definitions for tmax-node (legacy, unused — may be removed)
tmax-host              ← relay/rendezvous host binary: message forwarding + endpoint lookup
tmax-mesh-tests        ← E2E integration tests for multi-node mesh scenarios
```

### Key patterns

- **Protocol-first**: all request/response shapes live in `tmax-protocol`. Changes there must stay backward-compatible and be reflected in server + all clients.
- **Transport limits**: `MAX_JSON_LINE_BYTES`, `MAX_OUTPUT_CHUNK_BYTES`, `MAX_INPUT_CHUNK_BYTES` are enforced at protocol level. Respect them.
- **EventBroker** (`libtmax/broker.rs`): per-session event channel lifecycle (register/remove/subscribe).
- **VT state** (`libtmax/vt_state.rs`): server-side terminal state parsed via `vte`, emits snapshots for reconnecting subscribers.
- **Socket security**: runtime dir `0700`, socket `0600`, peer UID checks. Preserve this.
- **Single-writer-per-socket**: server/client outbound streams must maintain this invariant.
- **Comms policy**: server supports `open`, `same_subtree`, `parent_only` hierarchy enforcement for mailbox/task routing.

### Release & Ops

- `scripts/package-release.sh` → produces `dist/tmax-<target>.tar.gz`
- `scripts/deploy-linux.sh` / `scripts/rollback-linux.sh` → idempotent deploy automation
- `ops/systemd/` → service unit, config, env files
- CI: GitHub Actions with Linux/macOS matrix, gates on `fmt`, `clippy -D warnings`, `cargo test`, package verification

## Non-Negotiable Constraints

- Keep protocol changes backward-aware across server and all clients
- Enforce transport limits from `tmax-protocol` constants
- Maintain single-writer-per-socket behavior
- Avoid unbounded queues in hot paths
- Preserve local socket security model

## Working Conventions

- The source-of-truth plan lives at `docs/plans/2026-02-14-feat-tmax-terminal-multiplexer-plan.md`
- Update AGENT.md and plan doc checkboxes when completing meaningful work
- Prefer incremental, tested slices over large speculative rewrites
- Leave no dead placeholder code in touched files
