---
status: complete
priority: p2
issue_id: "005"
tags: [code-review, security]
dependencies: []
---

# Unix Socket Created Without Permission Restrictions

## Problem Statement

The Unix socket is created without setting restrictive file permissions. It inherits from umask (commonly 0022), making it potentially accessible to other users on multi-user systems. The stale socket removal also doesn't verify file type/ownership (symlink attack vector).

**Flagged by:** security-sentinel

## Findings

- **File:** `crates/tmax-server/src/server.rs` lines 16-32
- No `chmod` after `UnixListener::bind`
- Fallback path `/tmp/tmax-{uid}.sock` is in shared directory
- Stale socket removed without checking if it's a symlink

## Proposed Solutions

### Option A: Set permissions to 0700 after bind (Recommended)
Use `std::fs::set_permissions` immediately after socket creation. Check file type before removing stale sockets.

**Pros:** Simple, effective
**Cons:** None
**Effort:** Small
**Risk:** Low

## Acceptance Criteria

- [ ] Socket permissions set to 0700 or 0600 after creation
- [ ] Stale socket verified as socket type before removal
- [ ] No symlink following on removal

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
