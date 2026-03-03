---
status: pending
priority: p2
issue_id: "026"
tags: [code-review, architecture]
dependencies: ["025"]
---

# Centralize Duplicated Arda Constants into Shared Crate

## Problem Statement

`ARDA_KEY_FILE` and `ARDA_DEFAULT_GATEWAY_URL` are independently defined as string constants in `agentbook-cli/src/login.rs` and `agentbook-tui/src/automation.rs`. A test asserts they match, but this is duct-tape — if someone changes one and forgets the test, runtime behavior diverges silently.

## Findings

- **File:** `crates/agentbook-tui/src/automation.rs:20-22` — duplicated constants
- **File:** `crates/agentbook-cli/src/login.rs:33-37` — same constants
- **File:** `crates/agentbook-tui/src/automation.rs:978-983` — test asserting equality (duct-tape)
- **Source:** code-simplicity-reviewer, agent-native-reviewer, performance-oracle agents (all flagged)

## Proposed Solutions

### Solution A: Move to agentbook (shared lib crate) (Recommended)
- Both `agentbook-cli` and `agentbook-tui` depend on the `agentbook` shared lib
- Define constants there and import from both crates
- Delete the duct-tape matching test
- **Effort:** Small | **Risk:** Low

### Solution B: Move to agentbook-mesh
- `agentbook-mesh` already provides `state_dir::default_state_dir()`
- Co-locating the file name constants with the state_dir logic is natural
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [ ] Constants defined once in shared crate
- [ ] Both CLI and TUI import from shared crate
- [ ] Duct-tape matching test deleted
- [ ] All tests pass

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-03 | Created from code review | - |
