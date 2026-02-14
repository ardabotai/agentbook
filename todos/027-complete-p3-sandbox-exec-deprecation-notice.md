---
status: complete
priority: p3
issue_id: "027"
tags: [code-review, security, sandbox]
dependencies: []
---

# Add sandbox-exec Deprecation Notice and Enforcement Verification

## Problem Statement

Apple has deprecated `sandbox-exec` and the seatbelt profile language. There is no programmatic way to verify the sandbox was successfully applied. If `sandbox-exec` silently fails on a future macOS version, the process runs completely unsandboxed.

## Findings

- **Source**: Security Sentinel
- **File**: `crates/tmax-sandbox/src/macos.rs`

## Proposed Solutions

1. Log a warning that sandbox-exec is deprecated
2. Add a runtime check that verifies sandbox enforcement (attempt a denied write from child)
3. Document migration plan to App Sandbox / Containerization framework

- Effort: Medium
- Risk: Low

## Acceptance Criteria

- [ ] Warning logged about deprecation
- [ ] Documentation of known limitations and migration path

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | |
