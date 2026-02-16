# AGENT.md

## Project Purpose
`tmax` is a programmable terminal multiplexer for AI workflows.
Core transport is JSON-lines over Unix sockets.

## Workspace Layout
- `crates/tmax-protocol`: shared protocol types/constants
- `crates/tmax-agent-sdk`: high-level agent workflow client (`run_task`, `tail_task`, `cancel_task`)
- `crates/libtmax`: PTY/session core
- `crates/tmax-local`: daemon over Unix socket
- `crates/tmax-cli`: CLI client
- `crates/tmax-web`: web bridge (Phase 1)
- `crates/tmax-sandbox`: sandboxing (Phase 2)
- `crates/tmax-git`: git integration (Phase 3)
- `crates/tmax-client`: native terminal client (Phase 4)

## Current Status
- Phase 0 core engine baseline is implemented and tested (`cargo check`, `cargo test`).
- Phase 0 server integration coverage now includes end-to-end create/stream, multi-subscriber replay, edit-vs-view enforcement, and one-client multi-session subscription framing.
- Phase 0 socket hardening tests now validate runtime/socket permission modes and peer-uid rejection behavior.
- Phase 0 CLI integration coverage now includes mock-server request/response tests for `list` and `new`, plus argument validation for missing `--exec/--shell`.
- Phase 1 is implemented with baseline + hardening: REST/WS bridge, batching, backpressure, quotas, CORS.
- Phase 1 integration coverage now includes end-to-end WebSocket streaming and reconnect `last_seq` catch-up tests.
- Phase 2 baseline is implemented: sandbox scope normalization + parent/child scope enforcement.
- Phase 2 hardening includes canonicalized scope resolution and symlink-escape rejection tests for nested sandbox validation.
- Phase 2 macOS runtime path now uses `sandbox-exec` for sandboxed command spawns, with tests showing inside-scope writes succeed and outside writes are blocked.
- Phase 2 Linux path now uses `tmax-sandbox-runner` (`nix` namespaces + mount/bind remount) for sandboxed spawns; Linux integration tests verify inside-scope writes succeed and outside-scope writes fail when user namespaces are available.
- macOS Containerization FFI feasibility research is documented in `docs/research/2026-02-14-macos-containerization-ffi.md`; current runtime backend remains `sandbox-exec` until a Swift shim exists.
- Phase 3 baseline is implemented: git repo/worktree metadata detection surfaced in session summaries.
- Phase 4 baseline is implemented: minimal native `tmax-client` (attach/view/edit stream with raw input forwarding).
- Phase 4 VT foundation is implemented: server output is parsed via `vte` into a terminal screen buffer and rendered in the native client.
- Phase 4 pane layout foundation is implemented: horizontal/vertical split + resize engine with layout tests is in place.
- Phase 4 interaction now includes brainstorm keybindings (`c/n/p/1-9`, `|/-`, `h/j/k/l`, `d`, `/`, `m`, `w`, `?`), local window switching, scroll mode, regex search highlight, marker jumps, and smooth wheel scrolling.
- Phase 4 integration coverage now includes a headless `tmax-client` protocol smoke test (session info + attach + detach sequence).
- Session summaries now include explicit `sandboxed` metadata for downstream clients.
- Transport limit coverage now includes explicit oversized input rejection tests in `libtmax`.
- CI baseline is now in-repo with Linux/macOS matrix and strict gates (`fmt`, `clippy -D warnings`, `cargo test`).
- Optional append-only per-session `HistoryLog` capture is now wired in `libtmax` (configurable via `TMAX_HISTORY_DIR`) with test coverage.
- Session metadata now includes explicit user tags end-to-end (protocol, server, CLI, and summaries).
- Core now maintains server-side VT state and emits snapshot events for new/stale subscribers with tested reconnect resync behavior.
- Core now includes an explicit `EventBroker` for per-session event channel lifecycle (register/remove/subscribe ownership by session ID).
- PTY spawn path now includes short retry/backoff to tolerate transient PTY allocation failures under test/load.
- Load/backpressure coverage now includes a multi-subscriber stress test in normal CI test runs.
- Phase 0 perf smoke coverage now includes integration measurements for session create latency, output stream latency, and RSS delta (`integration_perf_smoke_create_and_stream_latency`).
- Session runtime now uses a single per-session PTY IO+wait helper thread (instead of separate IO/wait helpers), smaller helper thread stacks, and reduced default live-buffer/broadcast capacities for lower memory overhead.
- Release packaging now has a single-artifact script at `scripts/package-release.sh` (produces `dist/tmax-<target>.tar.gz` with all role binaries).
- CI workflow has been corrected and expanded with per-OS target compile checks.
- Phase 3 integration coverage now includes `tmax-git` worktree lifecycle integration tests.
- Native Ubuntu x86_64 validation has been completed over SSH with strict gates (`fmt`, `clippy -D warnings`, full workspace tests) passing.
- Perf smoke on native Ubuntu x86_64 now reports memory within target (`rss_delta_kb=4992`), satisfying the `<5MB` session memory gate.
- RC1 release outputs are prepared: artifacts for macOS/Linux, checksum file, machine-readable manifest, and release runbook (`docs/releases/2026-02-14-rc1.md`).
- Post-RC hardening Step 1 is complete: `ops/systemd` service assets, `tmax-cli health`, CLI health integration tests, packaging inclusion of ops assets, and systemd deployment documentation.
- Post-RC hardening Step 2 is complete: idempotent Linux deploy/rollback automation scripts with health-gated failure behavior.
- Post-RC hardening Step 3 includes CI package-content verification for required ops assets.
- Post-RC hardening Step 4 is complete: agent-first SDK crate (`tmax-agent-sdk`) and high-level CLI flows (`run-task`, `tail-task`, `cancel-task`) that hide manual attach/subscribe bookkeeping.
- Post-RC hardening Step 5 is complete: SDK now includes structured error classes, retry policies, timeout/cancel task execution (`execute_task_and_collect`), resumable tail helper, and operations wrappers (`run_deploy`, `run_rollback`, `wait_ready`), with CLI retry/timeout flags wired for task commands.
- Added first-class inter-agent communication and shared work tracking primitives: protocol-level mailbox/task requests and events, `libtmax` inbox/task state management, server routing, CLI command groups (`msg`, `tasks`), SDK helper methods, and integration test coverage across crates.
- Added connection-scoped session awareness for mailbox/task calls in `tmax-local`: when a connection is attached, sender identity can be inferred from bound session(s) and cross-session inbox/task access is denied unless the session is bound on that connection.
- Added hierarchy-aware communication policy controls in `tmax-local` (`open`, `same_subtree`, `parent_only`) with enforcement for mailbox routes and task claim/status peer checks, plus integration coverage for sibling-deny behavior under `parent_only`.
- The source of truth plan is:
  `docs/plans/2026-02-14-feat-tmax-terminal-multiplexer-plan.md`

## Remaining Production Gates
- Run Step 2 automation end-to-end on Ubuntu host and record evidence (currently blocked by password-required sudo on host).
- Record a rollback drill result in release notes.

## Phase Gate Rule
- Before moving to the next phase, update both this file and the plan checklist.
- Mark completed items with evidence (tests/smoke command) and keep unknown items unchecked.

## Non-Negotiable Constraints
- Keep protocol changes backward-aware. If changing request/response shape, update both server and CLI/web.
- Enforce transport limits from `tmax-protocol` constants.
- Keep single-writer-per-socket behavior for server/client outbound streams.
- Avoid unbounded queues in hot paths.
- Preserve local socket security (`0700` runtime dir, `0600` socket, peer uid checks).

## Build and Test Commands
Run from the repository root:

```bash
cargo check
cargo test
```

## Smoke Commands
Start server:

```bash
cargo run -p tmax-local
```

In another shell:

```bash
cargo run -p tmax-cli -- new 'echo hello'
cargo run -p tmax-cli -- list
cargo run -p tmax-cli -- run-task --timeout-ms 10000 'echo hello from run-task'
cargo run -p tmax-cli -- stream <session-id> --last-seq 0
cargo run -p tmax-cli -- down
```

> Socket path is auto-discovered. Use `--socket <path>` only for custom locations.

## Delivery Expectations for Agents
- Update the plan doc checkboxes/log when you complete meaningful work.
- Prefer incremental, tested slices over large speculative rewrites.
- Leave no dead placeholder code in touched files.
