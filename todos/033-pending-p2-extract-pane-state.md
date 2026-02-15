---
status: pending
priority: p2
issue_id: "033"
tags: [code-review, architecture, tmax-client]
dependencies: []
---

# Event Loop run() Is a 200-line Monolith

## Problem Statement

The `run()` function in `event_loop.rs` is ~200 lines with vt100 parser, screen clone, terminal dimensions, and status bar rendering all as local variables. This makes it hard to extend for Phase 4.2 multi-pane support.

## Findings

- **event_loop.rs**: 200+ lines in a single function
- vt100 parser state managed as loose locals
- Status bar render called from 4 different places with identical args
- Architecture review recommends extracting `PaneState` struct

## Proposed Solutions

### Option A: Extract PaneState struct (Recommended)
Move parser, prev_screen, dimensions into a `PaneState` struct with methods for processing output and rendering.
- Effort: Medium | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/event_loop.rs`

## Acceptance Criteria
- [ ] PaneState struct encapsulates vt100 state
- [ ] run() function is shorter and more readable
- [ ] All existing tests pass
