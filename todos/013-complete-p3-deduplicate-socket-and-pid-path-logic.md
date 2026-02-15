---
status: complete
priority: p3
issue_id: "013"
tags: [code-review, quality]
dependencies: []
---

# Deduplicate Socket and PID Path Logic

## Problem Statement

Socket path resolution is duplicated between `tmax-server/config.rs` and `tmax-cli/client.rs`. PID file path logic is duplicated between `tmax-server/config.rs` and `tmax-cli/commands.rs`. These can drift apart causing connection failures.

**Flagged by:** architecture-strategist, code-simplicity-reviewer

## Findings

- `crates/tmax-server/src/config.rs` lines 28-35 vs `crates/tmax-cli/src/client.rs` lines 69-76
- `crates/tmax-server/src/config.rs` lines 37-39, 51-58 vs `crates/tmax-cli/src/commands.rs` lines 460-473

## Proposed Solutions

### Option A: Move shared path logic to `tmax-protocol` (Recommended)
Add a `paths` module to `tmax-protocol` with `socket_path()` and `pid_file_path()`.

**Pros:** Single source of truth, prevents drift
**Cons:** Protocol crate gains `libc` dep (for getuid)
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] Socket/PID path logic exists in one place
- [ ] Both server and CLI use the shared logic
- [ ] All tests pass

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
