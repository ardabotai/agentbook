---
status: done
priority: p3
issue_id: "016"
tags: [code-review, quality]
dependencies: []
---

# Remove Dead Code and YAGNI Violations

## Problem Statement

Several pieces of code exist but have no production callers:
- `canonical_message_payload` and `append_length_prefixed` in crypto.rs (built for future signing scheme)
- `derive_pairwise_key` in crypto.rs (duplicated by identity.rs inline ECDH)
- `agentbook-tests` crate (70 lines of unused scaffolding)
- Benchmark file copies 100+ lines of router internals

## Findings

- **Simplicity Agent:** Identified ~332 lines of removable dead code/YAGNI

## Proposed Solutions

### Clean Up
- **Effort:** Small
- Remove `canonical_message_payload`, `append_length_prefixed` (or wire them in when encryption is implemented)
- Align `derive_pairwise_key` with `identity.rs` usage or remove
- Delete `agentbook-tests` crate until E2E tests are written
- Refactor bench to import from router.rs instead of copy-pasting

## Acceptance Criteria

- [x] No dead code in production paths
- [x] All tests pass
- [x] Clippy clean

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by simplicity review agent |
| 2026-02-16 | Completed | Removed dead functions from crypto.rs, deleted agentbook-tests crate, removed unused thiserror workspace dep. Bench left as-is (lower priority). |
