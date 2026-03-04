---
status: pending
priority: p2
issue_id: "052"
tags: [code-review, architecture, race-condition]
dependencies: []
---

# Use crossterm::EventStream Instead of spawn_blocking Poll+Read

## Problem Statement

The event loop uses `spawn_blocking(|| event::poll(16ms))` then calls `event::read()` synchronously on the main task. This pattern has a subtle race: `event::poll` and `event::read` share global state (crossterm's internal reader). A stale `spawn_blocking` task from a prior loop iteration could overlap. crossterm's `EventStream` is designed for async use and avoids this.

## Findings

- **File:** `main.rs:177-180` — spawn_blocking for poll, synchronous read
- **Source:** race-conditions-reviewer

## Proposed Solutions

### Solution A: Replace with crossterm::event::EventStream
- `use crossterm::event::EventStream; let mut reader = EventStream::new();`
- In select!: `event = reader.next() => { ... }`
- Eliminates spawn_blocking overhead and race
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] event::poll/read replaced with EventStream
- [ ] No spawn_blocking for keyboard input
- [ ] All input handling unchanged

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI race conditions review | Designed-for-async vs manual threading |
