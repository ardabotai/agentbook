---
status: pending
priority: p1
issue_id: "043"
tags: [code-review, performance, race-condition, robustness]
dependencies: []
---

# Move Blocking Operations Off the Event Loop

## Problem Statement

Three blocking operations run on the main async event loop, freezing the UI:
1. `automation::tick()` → `decide_pi()` → `run_command_with_stdin()` blocks for up to 6 seconds during PI inference
2. `detect_waiting_input_windows()` → `collect_tab_snapshots()` spawns synchronous tmux subprocesses (20-80ms per scan, every 1.2s)
3. `collect_tab_snapshots()` in tick() also spawns blocking tmux calls

During these blocks, no keyboard input, rendering, or socket reads occur.

## Findings

- **File:** `main.rs:378` — `automation::tick(app)` called every loop iteration
- **File:** `automation.rs:90-96` — tick calls collect_tab_snapshots (blocking tmux)
- **File:** `automation.rs:688-735` — run_command_with_stdin uses mpsc::recv_timeout (blocking)
- **File:** `main.rs:346-365` — prompt_scan_interval calls detect_waiting_input_windows (blocking tmux)
- **Source:** performance-oracle, race-conditions-reviewer

## Proposed Solutions

### Solution A: Move PI inference to background thread, poll for results
- Match the pattern already used for streaming (start_pi_chat_stream)
- tick() only checks if a result is ready, never blocks
- **Effort:** Medium | **Risk:** Low

### Solution B: Move all tmux calls to spawn_blocking
- Wrap collect_tab_snapshots and detect_waiting_input_windows in tokio::task::spawn_blocking
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] PI inference runs in background, does not block UI
- [ ] tmux subprocess calls do not block the event loop
- [ ] UI remains responsive during all automation operations

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI robustness review | Prevents 6-second UI freezes |
