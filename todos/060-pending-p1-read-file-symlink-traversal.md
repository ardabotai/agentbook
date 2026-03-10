---
status: done
priority: p1
issue_id: "060"
tags: [code-review, security, path-traversal, pi-terminal-agent]
dependencies: []
---

# Fix read_file Path Traversal Bypass via Symlinks

## Problem Statement

The `sanitizeRequestedPath` function in `pi-terminal-agent.mjs` uses string-based `path.relative` to check for `..` traversal. This is bypassed by symlinks: if a symlink inside the workspace points outside it (e.g., `./link -> /etc/passwd`), the check passes but `readFile` follows the symlink and reads the external file. Since the LLM requests these paths, prompt injection in terminal output could trigger arbitrary file reads.

## Findings

- **File:** `agent/scripts/pi-terminal-agent.mjs:641-651` — `sanitizeRequestedPath` does string-only check
- **File:** `agent/scripts/pi-terminal-agent.mjs:660` — `readFile(pathCheck.absPath)` follows symlinks
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Resolve realpath before check (recommended)
- Use `fs.realpath` on the resolved absolute path, then verify the real path starts with `fsRoot`
- **Effort:** Small | **Risk:** Low (may reject valid relative symlinks within workspace)

### Solution B: lstat to reject external symlinks
- Use `fs.lstat` to detect symlinks, then check `fs.readlink` target is within workspace
- **Effort:** Small | **Risk:** Low

## Acceptance Criteria

- [x] Symlinks pointing outside workspace are rejected by `sanitizeRequestedPath`
- [x] Regular files and intra-workspace symlinks still work
- [x] Test case: symlink to `/etc/passwd` returns error

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | prompt injection + symlink = arbitrary file read |
| 2026-03-10 | Fixed: added realpathSync check in sanitizeRequestedPath | realpathSync resolves symlinks; falls back gracefully for non-existent files |
