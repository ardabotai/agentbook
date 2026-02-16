---
status: done
priority: p1
issue_id: "002"
tags: [code-review, security]
dependencies: ["001"]
---

# Enable Ingress Validation on Inbound Messages

## Problem Statement

`process_inbound` in handler.rs performs **zero validation** on incoming messages. No signature verification, no follow-graph enforcement for DMs, no block checking, no rate limiting. An `IngressPolicy` module exists in `agentbook-mesh/src/ingress.rs` -- fully implemented and tested -- but is never called.

## Findings

- **Security Agent (HIGH-001):** Any node or the relay itself can forge messages appearing to come from any sender.
- **Architecture Agent:** `IngressPolicy` has 4 passing tests covering accept, reject-unfollowed, reject-blocked, reject-bad-signature.

## Proposed Solutions

### Option A: Wire IngressPolicy into process_inbound
- **Effort:** Small
- **Risk:** Low (code exists and is tested)
- Create `IngressPolicy` from `FollowStore` + identity
- Call `policy.check(&envelope)` before processing
- Reject messages that fail validation

## Acceptance Criteria

- [ ] `IngressPolicy::check()` called for every inbound envelope
- [ ] DMs from non-followers are rejected
- [ ] Messages from blocked nodes are rejected
- [ ] Invalid signatures are rejected
- [ ] Rate limiting active on inbound messages

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by security and architecture review agents |
