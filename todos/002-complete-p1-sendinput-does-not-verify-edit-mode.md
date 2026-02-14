---
status: pending
priority: p1
issue_id: "002"
tags: [code-review, security, bug]
dependencies: []
---

# SendInput Does Not Verify Edit Mode

## Problem Statement

The `SendInput` handler checks whether the client has *any* attachment to the session, but does not verify the attachment is in `Edit` mode. A client attached in `View` mode can send arbitrary input to the PTY, bypassing the intended access control boundary.

**Flagged by:** security-sentinel, architecture-strategist, code-simplicity-reviewer

## Findings

- **File:** `crates/tmax-server/src/connection.rs` lines 220-243
- `ClientState.attachments` stores `(SessionId, String)` -- no `AttachMode` stored
- Variable named `has_edit` but actually checks for any attachment
- View/Edit mode distinction is a core security boundary that is not enforced

## Proposed Solutions

### Option A: Store AttachMode in ClientState (Recommended)
Change `attachments` from `Vec<(SessionId, String)>` to `Vec<(SessionId, String, AttachMode)>` and check mode in SendInput.

**Pros:** Simple one-line logic fix, minimal code change
**Cons:** None
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] `ClientState.attachments` includes `AttachMode`
- [ ] `SendInput` rejects requests from `View` attachments
- [ ] Test added verifying view-only clients cannot send input

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
