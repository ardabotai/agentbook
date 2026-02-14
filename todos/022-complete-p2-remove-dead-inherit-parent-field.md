---
status: complete
priority: p2
issue_id: "022"
tags: [code-review, architecture, sandbox]
dependencies: []
---

# Remove Dead `inherit_parent` Field

## Problem Statement

The `inherit_parent` field on `SandboxConfig` exists but is never read in any logic. `resolve_sandbox_nesting()` always inherits the parent sandbox when the child has none, regardless of `inherit_parent`. The field is set to `true` everywhere and never branched on. A user setting `inherit_parent: false` would expect their child to run unsandboxed, but it will still be sandboxed.

## Findings

- **Source**: Architecture Strategist, Code Simplicity Reviewer
- **Files**: `crates/tmax-protocol/src/lib.rs:163`, `crates/libtmax/src/session.rs:132-136`

## Proposed Solutions

### Solution A: Remove the field entirely (Recommended)
Parent sandboxes are always mandatory for children. Remove the field and the misleading documentation.

- Pros: Honest API, no dead code, simpler
- Cons: Minor breaking change to wire protocol
- Effort: Small
- Risk: Low

## Acceptance Criteria

- [ ] `inherit_parent` removed from `SandboxConfig`
- [ ] All references updated (tests, CLI, docs)
- [ ] Protocol roundtrip tests updated

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | |
