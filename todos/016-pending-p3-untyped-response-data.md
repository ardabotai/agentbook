---
status: pending
priority: p3
issue_id: "016"
tags: [code-review, architecture]
dependencies: []
---

# Response Data is Untyped serde_json::Value

## Problem Statement

`Response::Ok { data: Option<serde_json::Value> }` forces CLI to do runtime field access like `data["session_id"].as_str()`. Protocol changes silently break clients at runtime instead of compile time.

**Flagged by:** architecture-strategist

## Findings

- **File:** `crates/tmax-protocol/src/lib.rs` lines 82-93
- CLI does fragile runtime JSON field access throughout `commands.rs`
- No compile-time safety for protocol evolution

## Proposed Solutions

### Option A: Typed response variants per command
Create specific response types (e.g., `SessionCreateResponse`, `SessionListResponse`).

**Pros:** Compile-time safety, self-documenting protocol
**Cons:** More types to maintain, protocol change
**Effort:** Medium
**Risk:** Medium (wire format change)

## Acceptance Criteria

- [ ] Response variants are typed
- [ ] CLI uses typed deserialization
- [ ] All tests pass

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
