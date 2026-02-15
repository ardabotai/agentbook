---
status: complete
priority: p3
issue_id: "041"
tags: [code-review, performance, simplicity, tmax-client]
dependencies: []
---

# Status Bar Re-rendered Unconditionally on Every Keypress

## Problem Statement

Despite a comment saying "Re-render status bar if mode changed", the status bar is re-rendered on every keypress regardless. This involves cursor movement, attribute changes, string formatting, and padding computation for zero visual change most of the time.

## Findings

- **event_loop.rs:101-112**: Status bar rendered on every key event
- Comment contradicts code behavior
- Also involves Vec allocation, format!, and join per render (status_bar.rs:27-44)

## Proposed Solutions

### Option A: Track mode change (Recommended)
Compare mode before/after `handle_key()`, only re-render on change.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/event_loop.rs`

## Acceptance Criteria
- [ ] Status bar only re-renders when input mode changes
- [ ] PREFIX indicator still appears/disappears correctly
