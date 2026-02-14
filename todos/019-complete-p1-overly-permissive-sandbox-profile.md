---
status: complete
priority: p1
issue_id: "019"
tags: [code-review, security, sandbox]
dependencies: []
---

# Overly Permissive Sandbox Profile - (allow default)

## Problem Statement

The seatbelt profile begins with `(allow default)` which permits everything except what is explicitly denied. The profile only denies `file-write*`. This means sandboxed processes can still: open network connections, execute arbitrary binaries, read any file on the system, send signals to other processes, and use IPC mechanisms.

For a terminal multiplexer where sessions may contain sensitive work, this defeats the purpose of sandboxing.

## Findings

- **Source**: Security Sentinel agent
- **File**: `crates/tmax-sandbox/src/macos.rs:12`
- **Severity**: CRITICAL - sandbox provides minimal actual protection
- **Evidence**: `(version 1)\n(allow default)\n(deny file-write*)\n` only restricts writes

## Proposed Solutions

### Solution A: Add targeted denials for network and process-exec (Recommended)
Keep `(allow default)` but add explicit denials for the most dangerous capabilities.

```scheme
(version 1)
(allow default)
(deny file-write*)
(deny network*)
(allow file-write* (subpath "/dev"))
;; per-path allowances...
```

- Pros: Minimal change, blocks data exfiltration, maintains compatibility
- Cons: Still allows file reads and other operations
- Effort: Small
- Risk: Low (may need to allow specific network ops if child processes need DNS)

### Solution B: Full deny-default profile
Switch to `(deny default)` and explicitly allow only what's needed.

- Pros: Maximum security, principle of least privilege
- Cons: Will break many programs that need network, mach ports, sysctl, etc. Requires extensive testing
- Effort: Large
- Risk: High (may be too restrictive for general terminal use)

### Solution C: Document the limitation
Document that the sandbox only restricts filesystem writes and does not prevent network access or code execution.

- Pros: Zero code change, honest about capabilities
- Cons: Does not improve security
- Effort: Small
- Risk: None

## Recommended Action

Solution A - add `(deny network*)` as a practical improvement. Consider making network denial configurable via `SandboxConfig` in a follow-up.

## Technical Details

- **Affected files**: `crates/tmax-sandbox/src/macos.rs`

## Acceptance Criteria

- [ ] Network access denied in seatbelt profile by default
- [ ] Integration test: sandboxed process cannot make network connections
- [ ] Document remaining sandbox limitations

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | sandbox-exec (allow default) is very permissive |

## Resources

- PR #2: https://github.com/ardabotai/tmax/pull/2
