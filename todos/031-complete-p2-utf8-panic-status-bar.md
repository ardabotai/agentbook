---
status: complete
priority: p2
issue_id: "031"
tags: [code-review, security, robustness, tmax-client]
dependencies: []
---

# Status Bar Byte-Level Truncation Panics on Multi-byte UTF-8

## Problem Statement

The status bar truncation `content[..cols as usize]` operates on byte indices. If the content contains multi-byte UTF-8 characters (from session labels or git branch names) and the terminal is narrow enough to trigger truncation, Rust will panic at runtime, crashing the client.

## Findings

- **status_bar.rs:57**: `content[..cols as usize].to_string()` — byte-level slicing
- Also flagged by security review (Finding 003) and architecture review (F-06)
- Similarly, **status_bar.rs:22**: `&session_id[..8]` could panic if session_id is < 8 bytes of a multi-byte string

## Proposed Solutions

### Option A: Character-aware truncation (Recommended)
Use `content.chars().take(cols as usize).collect::<String>()`.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/status_bar.rs`

## Acceptance Criteria
- [ ] Non-ASCII session labels don't crash the client
- [ ] Truncation works correctly with multi-byte characters
- [ ] Add test with non-ASCII characters
