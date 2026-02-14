---
status: complete
priority: p2
issue_id: "024"
tags: [code-review, documentation, sandbox]
dependencies: []
---

# Fix Stale Doc Comment About /private/var/folders

## Problem Statement

The doc comment on `generate_profile` in `macos.rs` states "5. Allows writes to /private/var/folders (for macOS temp dirs)" but the implementation does NOT include this path. The doc is misleading.

Additionally, programs that use standard macOS temp directories (`$TMPDIR` -> `/private/var/folders/...`) will fail with silent write denials.

## Findings

- **Source**: Security Sentinel, Architecture Strategist, Code Simplicity Reviewer
- **File**: `crates/tmax-sandbox/src/macos.rs:10`

## Proposed Solutions

### Solution A: Remove the stale doc line (Recommended for now)
Remove point 5 from the doc comment. The `/private/var/folders` blanket allow was intentionally removed because it was too broad (broke the denial integration test).

- Effort: Trivial
- Risk: None

### Solution B: Add scoped TMPDIR allow
Detect `$TMPDIR` at runtime and add a scoped allow for the current user's temp folder.

- Effort: Medium
- Risk: Low (but adds complexity)

## Acceptance Criteria

- [ ] Doc comment matches actual implementation

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | |
