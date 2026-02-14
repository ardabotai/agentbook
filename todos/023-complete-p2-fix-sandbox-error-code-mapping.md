---
status: complete
priority: p2
issue_id: "023"
tags: [code-review, architecture, sandbox]
dependencies: []
---

# Fix SandboxViolation Error Code Mapping

## Problem Statement

In `crates/libtmax/src/error.rs`, `TmaxError::SandboxViolation` maps to `ErrorCode::ServerError` instead of `ErrorCode::SandboxViolation`. This means clients cannot distinguish a sandbox violation from a generic server error.

## Findings

- **Source**: Architecture Strategist
- **File**: `crates/libtmax/src/error.rs:35`
- **Evidence**: `TmaxError::SandboxViolation(_) => (ErrorCode::ServerError, self.to_string())`

## Proposed Solutions

One-line fix:
```rust
TmaxError::SandboxViolation(_) => (ErrorCode::SandboxViolation, self.to_string()),
```

- Effort: Trivial
- Risk: None

## Acceptance Criteria

- [ ] `SandboxViolation` maps to `ErrorCode::SandboxViolation`
- [ ] Test confirms correct error code

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | |
