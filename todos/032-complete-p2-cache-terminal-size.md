---
status: complete
priority: p2
issue_id: "032"
tags: [code-review, performance, tmax-client]
dependencies: []
---

# Terminal Size Queried Redundantly Per Output Event

## Problem Statement

`event_loop.rs` calls `crossterm::terminal::size()` twice per output event (lines 172-173). Each call is an `ioctl(TIOCGWINSZ)` syscall. Terminal size only changes on Resize events, which are already handled separately.

## Findings

- **event_loop.rs:172-173**: Two `terminal::size()` calls per output event
- Also, `TerminalGuard::size()` is a trivial wrapper used once in main.rs while event_loop calls crossterm directly (inconsistent)

## Proposed Solutions

### Option A: Cache as mutable locals (Recommended)
Store `cols` and `content_rows` as mutable locals, update only in Resize arm.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/event_loop.rs`, `crates/tmax-client/src/terminal.rs`

## Acceptance Criteria
- [ ] Terminal size queried only on resize events
- [ ] Remove `TerminalGuard::size()` trivial wrapper
