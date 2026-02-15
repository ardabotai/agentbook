---
status: complete
priority: p3
issue_id: "039"
tags: [code-review, simplicity, dead-code, tmax-client]
dependencies: []
---

# Remove Dead git_branch Plumbing

## Problem Statement

`git_branch` is hardcoded to `None` in `event_loop.rs` and threaded through all 6 `render_status_bar` calls. The `git_branch` parameter in `status_bar.rs` and its conditional block can never fire. This adds noise at every call site.

## Findings

- **event_loop.rs:43-44**: `let git_branch: Option<String> = None;` — hardcoded
- **status_bar.rs:15,36-38**: Parameter and conditional that never execute
- Protocol on this branch doesn't support git_info
- ~15 LOC of dead plumbing across 2 files

## Proposed Solutions

### Option A: Remove git_branch entirely (Recommended)
Remove parameter from `render_status_bar`, remove variable from event_loop. Re-add when protocol supports it.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/event_loop.rs`, `crates/tmax-client/src/status_bar.rs`

## Acceptance Criteria
- [ ] git_branch parameter removed from render_status_bar
- [ ] All 6 call sites updated
- [ ] Tests updated to match new signature
