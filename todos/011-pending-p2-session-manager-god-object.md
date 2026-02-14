---
status: pending
priority: p2
issue_id: "011"
tags: [code-review, architecture]
dependencies: ["004"]
---

# SessionManager God Object (5+ Responsibilities)

## Problem Statement

`SessionManager` (803 lines) handles session lifecycle, PTY I/O, event brokerage, attachment management, marker management, and output buffering. This violates SRP and creates the single-lock bottleneck.

**Flagged by:** architecture-strategist

## Findings

- **File:** `crates/libtmax/src/session.rs` lines 109-633
- 6 distinct concerns in one struct
- Untestable without spawning real PTY processes (no trait abstraction)
- Directly creates the global lock contention issue

## Proposed Solutions

### Option A: Decompose into focused components
Split into `SessionRegistry`, `AttachmentManager`, `PtyManager` + keep `EventBroker` as peer.

**Pros:** SRP compliance, enables per-session locking, testability
**Cons:** Large refactor
**Effort:** Large
**Risk:** Medium

## Acceptance Criteria

- [ ] SessionManager split into focused components
- [ ] Each component independently lockable
- [ ] PTY abstracted behind trait for testability

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
