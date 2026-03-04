---
status: done
priority: p1
issue_id: "042"
tags: [code-review, performance, tui-rendering]
dependencies: []
---

# Coalesce Terminal Span Allocation in Draw Path

## Problem Statement

The terminal rendering in `ui.rs` creates one `Span` object per cell per frame. For a 200x50 terminal, that's 10,000 Span heap allocations every 16ms (60fps) — approximately 600,000 allocations per second. With split panes (up to 4), this becomes 2.4M allocations/sec, causing frame drops and GC pressure.

## Findings

- **File:** `ui.rs:771-791` — Cell-by-cell loop creating individual Spans
- **Source:** performance-oracle

## Proposed Solutions

### Solution A: Coalesce adjacent cells with identical styles into single Spans
- Build runs of same-style text, emit one Span per run
- Typically reduces span count by 10-50x
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [x] Terminal rendering uses coalesced spans (one per style run, not per cell)
- [x] Visual output identical to current implementation
- [x] Measurable reduction in allocations per frame

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-04 | Created from TUI performance review | Single highest-impact optimization |
| 2026-03-04 | Implemented run-length encoding in draw_terminal_pane | Coalesces adjacent same-style cells into single Spans |
