---
status: complete
priority: p3
issue_id: "025"
tags: [code-review, simplicity, sandbox]
dependencies: []
---

# Remove linux.rs YAGNI Stub

## Problem Statement

`crates/tmax-sandbox/src/linux.rs` is a 10-line file that does nothing except log a warning. The `#[cfg(not(target_os = "macos"))]` fallback in `command_prefix()` already handles unsupported platforms.

## Findings

- **Source**: Code Simplicity Reviewer
- **File**: `crates/tmax-sandbox/src/linux.rs`

## Proposed Solutions

Remove `linux.rs` entirely. Simplify the cfg gate to `#[cfg(not(target_os = "macos"))]`. When Linux sandboxing is actually implemented, create the module then.

- Effort: Trivial
- Risk: None

## Acceptance Criteria

- [ ] `linux.rs` removed
- [ ] cfg gate simplified
- [ ] Compiles on macOS

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | |
