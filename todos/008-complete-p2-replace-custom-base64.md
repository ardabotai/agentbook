---
status: complete
priority: p2
issue_id: "008"
tags: [code-review, quality, performance]
dependencies: []
---

# Replace Custom Base64 With `base64` Crate

## Problem Statement

87 lines of hand-rolled base64 encoding/decoding that silently discards `Write::write_all` errors and is 3-5x slower than the `base64` crate. Maintenance risk and unnecessary complexity.

**Flagged by:** security-sentinel, performance-oracle, code-simplicity-reviewer

## Findings

- **File:** `crates/tmax-protocol/src/lib.rs` lines 210-296
- Custom implementation with `let _ = result.write_all(...)` error suppression
- 128-byte lookup table with boundary check that could panic on invalid input
- Runs on every output event (hot path)

## Proposed Solutions

### Option A: Replace with `base64` crate (Recommended)
Two-line replacement using `base64::engine::general_purpose::STANDARD`.

**Pros:** -77 lines, battle-tested, SIMD-optimized, tiny dependency
**Cons:** Adds one dependency
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] Custom base64 module replaced with `base64` crate
- [ ] All serialization round-trip tests still pass
- [ ] No changes to wire format

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
