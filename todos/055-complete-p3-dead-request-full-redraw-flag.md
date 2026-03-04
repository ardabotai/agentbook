---
status: pending
priority: p3
issue_id: "055"
tags: [code-review, simplicity, dead-code]
dependencies: []
---

# Remove or Use request_full_redraw Dead Flag

## Problem Statement

`app.request_full_redraw` is set to `true` in ~30 places across the codebase but is never actually checked to decide whether to draw. The only draw gate is `last_draw.elapsed() >= min_draw_interval`. The flag is cleared before every draw but has no effect.

## Findings

- **File:** `main.rs:150` — flag cleared before draw but never checked
- **Source:** race-conditions-reviewer, code-simplicity-reviewer

## Proposed Solutions

### Solution A: Remove the flag entirely
- Delete all `request_full_redraw = true` assignments and the field
- **Effort:** Small | **Risk:** Low

### Solution B: Use as dirty flag to skip unnecessary draws
- Only draw when flag is true OR timer elapsed
- Could reduce CPU usage when UI is idle
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Flag either removed or actively used for draw decisions

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI review | Dead code cleanup |
