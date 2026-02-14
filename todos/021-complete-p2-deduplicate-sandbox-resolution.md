---
status: complete
priority: p2
issue_id: "021"
tags: [code-review, architecture, sandbox]
dependencies: []
---

# Deduplicate Sandbox Path Resolution

## Problem Statement

`ResolvedSandbox::resolve()` is called twice (or three times for child+parent) during `create_session()`: once in `resolve_sandbox_nesting()` for validation, then again in `create_session()` for the command prefix. Each call invokes `canonicalize()` syscalls redundantly.

## Findings

- **Source**: Performance Oracle, Architecture Strategist, Code Simplicity Reviewer (all three flagged this)
- **Files**: `crates/libtmax/src/session.rs:149-152` and `crates/libtmax/src/session.rs:211`

## Proposed Solutions

### Solution A: Return ResolvedSandbox from resolve_sandbox_nesting (Recommended)
Change `resolve_sandbox_nesting()` to return `Option<(SandboxConfig, ResolvedSandbox)>` so the already-resolved sandbox flows through to `create_session()`.

- Pros: Eliminates redundant syscalls, cleaner code, type-safe
- Cons: Minor refactor
- Effort: Small
- Risk: Low

Also tighten `validate_nesting` to accept `&ResolvedSandbox` instead of raw `&[PathBuf]`.

## Acceptance Criteria

- [ ] `resolve_sandbox_nesting` returns resolved sandbox
- [ ] No second `ResolvedSandbox::resolve()` call in `create_session()`
- [ ] All tests pass

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | |
