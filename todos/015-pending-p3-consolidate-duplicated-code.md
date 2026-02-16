---
status: done
priority: p3
issue_id: "015"
tags: [code-review, quality]
dependencies: []
---

# Consolidate Duplicated Code

## Problem Statement

Several utilities and patterns are duplicated across crates:
- `now_ms()` duplicated 4+ times
- `validate_username` duplicated in host/router.rs and cli/setup.rs with different return types
- `derive_kek_from_passphrase` in totp.rs duplicates `derive_key_from_passphrase` in recovery.rs
- HTTP endpoint formatting duplicated in handler.rs and transport.rs
- Two separate rate_limit.rs implementations (host: 326 lines, mesh: 84 lines)

## Findings

- **Simplicity Agent:** ~590 lines of potential reduction (~7% of codebase)
- **Pattern Agent:** Identifies 4 duplication clusters
- **Architecture Agent:** Confirms duplicate rate limiters and 1Password auth flows

## Proposed Solutions

### Consolidate to Shared Locations
- **Effort:** Small-Medium
- Add `pub fn now_ms()` to `agentbook-crypto` or shared crate
- Move `validate_username` to shared crate
- Remove `derive_kek_from_passphrase` from totp.rs, use crypto version
- Extract `connect_any_relay()` helper for relay connection retry
- Consolidate rate limiters (host version is superset)
- Remove unused `thiserror` workspace dependency or start using it

## Acceptance Criteria

- [x] No duplicated utility functions
- [x] Single rate limiter implementation
- [x] Single username validation implementation
- [x] All tests pass

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by simplicity and pattern review agents |
| 2026-02-16 | Completed | Consolidated now_ms, validate_username, rate_limit, derive_kek_from_passphrase into agentbook-crypto |
