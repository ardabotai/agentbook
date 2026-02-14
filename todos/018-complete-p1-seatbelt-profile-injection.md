---
status: complete
priority: p1
issue_id: "018"
tags: [code-review, security, sandbox]
dependencies: []
---

# Seatbelt Profile Injection via Crafted Writable Paths

## Problem Statement

The `generate_profile` function in `crates/tmax-sandbox/src/macos.rs` interpolates path strings directly into the seatbelt profile without sanitization. A path containing a double-quote character followed by arbitrary seatbelt directives can break out of the intended `subpath` filter and grant unrestricted file-write access.

Example: a path like `/tmp/x"))(allow default)(version 1)(allow file-write* (subpath "/"` would produce a profile that allows writes to the entire filesystem.

`canonicalize()` does NOT protect against this because macOS HFS+/APFS allow `"` in filenames.

## Findings

- **Source**: Security Sentinel agent
- **File**: `crates/tmax-sandbox/src/macos.rs:18-21`
- **Severity**: CRITICAL - complete sandbox escape
- **Evidence**: `format!("(allow file-write* (subpath \"{path_str}\"))\n")` with no validation

## Proposed Solutions

### Solution A: Validate paths after canonicalization (Recommended)
Reject paths containing characters with special meaning in seatbelt profile syntax: `"`, `(`, `)`, `\`.

```rust
fn validate_path_for_seatbelt(path: &Path) -> Result<(), SandboxError> {
    let s = path.to_string_lossy();
    let forbidden = ['"', '(', ')', '\\'];
    for ch in forbidden {
        if s.contains(ch) {
            return Err(SandboxError::InvalidPath(
                format!("path contains forbidden character '{}': {}", ch, s)
            ));
        }
    }
    Ok(())
}
```

- Pros: Simple, defensive, catches the attack vector
- Cons: Rejects legitimate (but unusual) paths with these characters
- Effort: Small
- Risk: Low

### Solution B: Shell-escape the path string
Use proper escaping for the seatbelt profile format.

- Pros: Allows all valid paths
- Cons: Seatbelt profile format is undocumented; escaping rules are uncertain
- Effort: Medium
- Risk: Medium (may miss edge cases in undocumented format)

## Recommended Action

Solution A - validate and reject paths with special characters.

## Technical Details

- **Affected files**: `crates/tmax-sandbox/src/macos.rs`, `crates/tmax-sandbox/src/lib.rs`
- **Add validation in**: `ResolvedSandbox::resolve()` after `canonicalize()` but before storing paths

## Acceptance Criteria

- [ ] Paths with `"`, `(`, `)`, `\` are rejected with `SandboxError::InvalidPath`
- [ ] Unit test: path with quote character is rejected
- [ ] Unit test: normal paths still pass validation

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-02-14 | Created from code review | Profile injection is a critical vector in sandbox-exec |

## Resources

- PR #2: https://github.com/ardabotai/tmax/pull/2
- Apple sandbox-exec seatbelt profile format (undocumented)
