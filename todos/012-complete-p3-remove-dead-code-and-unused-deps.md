---
status: pending
priority: p3
issue_id: "012"
tags: [code-review, quality, simplicity]
dependencies: []
---

# Remove Dead Code and Unused Dependencies

## Problem Statement

Several types, methods, and dependencies exist but are never used in production code. ~240 lines of dead weight.

**Flagged by:** code-simplicity-reviewer

## Findings

- `vte` dependency in `libtmax/Cargo.toml` -- unused
- `ClientCursor` and `ClientSubscriptions` -- never used in production (~50 lines)
- `EventBroker::with_capacity` and `has_channel` -- never called (~8 lines)
- `BrokerError` -- result always discarded (~5 lines)
- `Attachment.created_at` -- never read
- `OutputChunk.timestamp` -- never read
- `SessionManager::session_count` -- only used in tests
- `ServerConfig::default_buffer_size` -- `#[allow(dead_code)]`, never wired through
- `SandboxConfig` and `SandboxViolation` -- plumbed but never enforced

## Proposed Solutions

### Option A: Remove in one cleanup pass (Recommended)
Remove all dead code, unused deps, and document sandbox as unimplemented.

**Pros:** ~240 fewer lines, faster compile, cleaner API
**Cons:** Changes wire format if sandbox removed
**Effort:** Small
**Risk:** Low (keep sandbox in protocol, just document as unimplemented)

## Acceptance Criteria

- [ ] `vte` dependency removed
- [ ] Unused types and methods removed
- [ ] All tests still pass
- [ ] Clippy clean

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
