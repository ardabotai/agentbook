---
status: complete
priority: p3
issue_id: "038"
tags: [code-review, simplicity, yagni, tmax-client]
dependencies: []
---

# Remove Unused Mouse Capture

## Problem Statement

`EnableMouseCapture` and `DisableMouseCapture` are used in `terminal.rs`, but the event loop explicitly discards all mouse events with `Some(Ok(_)) => {}`. Mouse capture changes terminal behavior for zero benefit.

## Findings

- **terminal.rs:5,24,43**: Mouse capture imported and used in setup/teardown
- **event_loop.rs:155**: Mouse events silently discarded
- YAGNI violation — add mouse support when Phase 4.5 implements it

## Proposed Solutions

### Option A: Remove mouse capture (Recommended)
Remove `EnableMouseCapture`/`DisableMouseCapture` from terminal.rs.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/terminal.rs`

## Acceptance Criteria
- [ ] Mouse capture removed from setup and teardown
- [ ] All tests pass
