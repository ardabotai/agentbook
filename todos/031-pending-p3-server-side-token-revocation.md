---
status: deferred
priority: p3
issue_id: "031"
tags: [code-review, security]
dependencies: []
---

# Add Server-Side Token Revocation on Logout

## Problem Statement

`cmd_logout()` only deletes the local key file without revoking the token server-side. A stolen API key remains valid even after logout.

## Findings

- **File:** `crates/agentbook-cli/src/login.rs:101-118` — no revocation endpoint call
- **Source:** security-sentinel agent

## Proposed Solutions

### Solution A: Call revocation endpoint (best-effort)
- Read key before deleting, POST to `/api/v1/oauth/revoke`, then delete local files
- Best-effort: don't fail logout if revocation fails (server might be unreachable)
- Requires server-side revocation endpoint support
- **Effort:** Small (client) + Medium (server) | **Risk:** Low

## Acceptance Criteria

- [ ] Logout attempts server-side revocation before deleting local key
- [ ] Logout succeeds even if revocation fails (offline graceful degradation)

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | Requires Arda Gateway revocation endpoint |
