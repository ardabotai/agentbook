---
status: complete
priority: p2
issue_id: "020"
tags: [code-review, security, sandbox]
dependencies: []
---

# Narrow /dev Write Access in Seatbelt Profile

## Problem Statement

The seatbelt profile grants write access to the entire `/dev` tree via `(allow file-write* (subpath "/dev"))`. While `/dev/null` and `/dev/tty` are needed, `/dev` also contains raw disk devices (`/dev/disk*`), kernel memory (`/dev/kmem`), and other sensitive device files.

## Findings

- **Source**: Security Sentinel agent
- **File**: `crates/tmax-sandbox/src/macos.rs:15`

## Proposed Solutions

### Solution A: Use targeted literal/regex rules (Recommended)
Replace blanket `/dev` with specific device patterns needed for PTY operation.

```scheme
(allow file-write* (literal "/dev/null"))
(allow file-write* (literal "/dev/zero"))
(allow file-write* (regex #"^/dev/ttys[0-9]+$"))
(allow file-write* (regex #"^/dev/pty[a-z][0-9a-f]$"))
```

- Pros: Minimal attack surface
- Cons: May miss edge-case device files; needs testing
- Effort: Small
- Risk: Medium (may break if PTY device naming varies)

## Acceptance Criteria

- [ ] `/dev` blanket allow replaced with targeted rules
- [ ] Integration tests still pass (PTY operations work)
- [ ] Test that writes to `/dev/disk0` are denied

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | |
