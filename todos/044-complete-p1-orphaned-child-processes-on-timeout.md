---
status: pending
priority: p1
issue_id: "044"
tags: [code-review, race-condition, robustness, resource-leak]
dependencies: []
---

# Kill Orphaned Child Processes on Timeout

## Problem Statement

`run_command_with_stdin` spawns a child process via `sh -lc` and a thread to read its output. When the 6-second timeout fires, the function returns an error but neither the child process nor the thread is killed. The child continues consuming CPU/memory indefinitely. Repeated timeouts accumulate zombie processes.

Similarly, `start_pi_chat_stream` spawns a reader thread that has no timeout — if the PI process hangs, `chat_streaming` stays true forever and the Sidekick freezes.

## Findings

- **File:** `automation.rs:688-735` — run_command_with_stdin: thread + child spawned, timeout abandons both
- **File:** `automation.rs:460` — start_pi_chat_stream reader thread has no timeout or cancellation
- **Source:** race-conditions-reviewer

## Proposed Solutions

### Solution A: Share Child handle via Arc<Mutex<Option<Child>>>, kill on timeout
- Spawning thread stores Child in shared handle
- Timeout path calls child.kill()
- **Effort:** Small | **Risk:** Low

### Solution B: Add cancellation flag to streaming thread
- AtomicBool checked in reader loop
- Set on timeout or sidekick disable
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Child process is killed when timeout fires
- [ ] Streaming thread exits when sidekick is disabled or times out
- [ ] No zombie processes accumulate after repeated timeouts

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI race conditions review | Resource leak under repeated timeouts |
