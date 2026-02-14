---
title: "macOS Sandbox Seatbelt Security Hardening - Phase 2 Code Review"
date: 2026-02-14
category: security-issues
tags:
  - security
  - sandbox
  - seatbelt
  - macos
  - code-review
  - code-quality
modules:
  - tmax-sandbox
  - tmax-protocol
  - libtmax
  - tmax-cli
severity:
  - P1
  - P2
  - P3
status: resolved
problem_type:
  - security-vulnerability
  - code-quality
  - cleanup
---

# macOS Sandbox Seatbelt Security Hardening

## Problem Symptom

Multi-agent code review of Phase 2 (macOS sandboxing via `sandbox-exec`) identified 10 findings across security vulnerabilities, code quality issues, and cleanup tasks. The most critical: a seatbelt profile injection vulnerability that allowed complete sandbox escape via crafted file paths.

## Root Cause Analysis

The sandbox implementation had three classes of issues:

1. **Security vulnerabilities** in seatbelt profile generation: user-controlled paths were interpolated directly into profile syntax without sanitization, the profile used `(allow default)` without denying network access, and `/dev` write access was granted too broadly.

2. **Code quality drift**: sandbox paths were resolved multiple times (redundant syscalls), a dead `inherit_parent` field created a misleading API, error codes were mapped incorrectly, and documentation was stale.

3. **Cleanup debt**: a YAGNI Linux stub, missing deprecation notice for Apple's deprecated `sandbox-exec`, and duplicated test boilerplate.

## Investigation Steps

1. Ran 5 parallel review agents: security-sentinel, performance-oracle, architecture-strategist, code-simplicity-reviewer, learnings-researcher
2. Synthesized findings into 10 categorized todo files (018-027)
3. Identified dependencies between fixes using a wave-based execution plan
4. Executed fixes in 3 waves with parallel agents per wave

## Working Solution

### P1 Security Fixes

#### 018: Seatbelt Profile Injection Prevention

Paths containing `"`, `(`, `)`, `\` could inject arbitrary seatbelt directives. A path like `/tmp/x"))(allow default)(version 1)(allow file-write* (subpath "/"` would grant full filesystem write access.

**Fix:** Added validation after `canonicalize()` but before profile generation:

```rust
const SEATBELT_UNSAFE_CHARS: &[char] = &['"', '(', ')', '\\'];

fn validate_path_for_seatbelt(path: &Path) -> Result<(), SandboxError> {
    let s = path.to_string_lossy();
    if let Some(c) = s.chars().find(|c| SEATBELT_UNSAFE_CHARS.contains(c)) {
        return Err(SandboxError::InvalidPath(format!(
            "path contains unsafe character {c:?} for seatbelt profile: {s}"
        )));
    }
    Ok(())
}
```

File: `crates/tmax-sandbox/src/lib.rs`

#### 019: Network Denial in Sandbox Profile

Profile had `(allow default)` but no network restriction, allowing data exfiltration.

**Fix:** Added `(deny network*)` to the seatbelt profile:

```scheme
(version 1)
(allow default)
(deny file-write*)
(deny network*)
```

File: `crates/tmax-sandbox/src/macos.rs`

### P2 Quality Fixes

#### 020: Narrow /dev Write Access

Replaced blanket `(subpath "/dev")` with targeted device patterns:

```rust
profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");
profile.push_str("(allow file-write* (literal \"/dev/zero\"))\n");
profile.push_str("(allow file-write* (regex #\"^/dev/ttys[0-9]+$\"))\n");
profile.push_str("(allow file-write* (regex #\"^/dev/pty[a-z][0-9a-f]$\"))\n");
```

File: `crates/tmax-sandbox/src/macos.rs`

#### 021: Deduplicate Sandbox Resolution

Changed `resolve_sandbox_nesting()` to return `(SandboxConfig, ResolvedSandbox)` tuple, eliminating redundant `canonicalize()` syscalls:

```rust
fn resolve_sandbox_nesting(
    &self,
    config: &SessionCreateConfig,
) -> Result<Option<(SandboxConfig, ResolvedSandbox)>, TmaxError>
```

Files: `crates/libtmax/src/session.rs`, `crates/tmax-sandbox/src/lib.rs`

#### 022: Remove Dead `inherit_parent` Field

Removed the unused field from `SandboxConfig` and all references. The field was never read in logic - parent sandbox was always inherited regardless of the value.

Files: `crates/tmax-protocol/src/lib.rs`, `crates/libtmax/src/session.rs`, `crates/libtmax/tests/sandbox_integration.rs`

#### 023: Fix Error Code Mapping

One-line fix: `SandboxViolation` mapped to `ErrorCode::SandboxViolation` instead of `ErrorCode::ServerError`.

File: `crates/libtmax/src/error.rs`

#### 024: Fix Stale Doc Comment

Removed false claim about `/private/var/folders` being allowed in the sandbox profile.

File: `crates/tmax-sandbox/src/macos.rs`

### P3 Cleanup Fixes

#### 025: Remove Linux Stub

Deleted YAGNI `linux.rs` module, simplified cfg gate to `#[cfg(not(target_os = "macos"))]` fallback.

Files: Deleted `crates/tmax-sandbox/src/linux.rs`, updated `crates/tmax-sandbox/src/lib.rs`

#### 026: Consolidate Test Boilerplate

Extended `sandboxed_session_config()` helper to accept `Option<SandboxConfig>` and `Option<SessionId>`, added `sandbox_with()` convenience function. All 5 tests now use shared helpers (~40 LOC reduction).

File: `crates/libtmax/tests/sandbox_integration.rs`

#### 027: Deprecation Notice

Added `tracing::warn!` for sandbox-exec deprecation in `command_prefix()`.

File: `crates/tmax-sandbox/src/lib.rs`

## Execution Strategy

Dependencies required wave-based execution:

```
Wave 1 (parallel): 018, 019+020+024, 021, 023, 025, 027
Wave 2 (after 021): 022
Wave 3 (after 022): 026
```

- Related changes to the same file (019+020+024 all modify `macos.rs`) grouped into single agent
- Trivial one-line fix (023) applied directly without spawning agent
- All 48 workspace tests pass after each wave

## Prevention Strategies

### Security: Sandbox Profile Generation

- **Validate all paths** for special characters before interpolating into profiles
- **Default-deny baseline**: use `(deny network*)` and explicit device allowlists
- **Type-safe profile builder**: consider structured AST instead of string interpolation
- **Fuzz testing**: generate paths with special characters, verify no injection

### Quality: Code Consistency

- **Single resolution pattern**: resolve paths once, pass `ResolvedSandbox` through the call chain
- **Centralized error mapping**: single `to_error_code()` method as source of truth
- **Compiler enforcement**: treat dead code warnings as errors in CI
- **Doc tests**: verify documentation examples compile and match implementation

### Cleanup: Technical Debt

- **YAGNI discipline**: no platform stubs for unsupported platforms
- **Deprecation workflow**: log warnings for deprecated APIs, document migration path
- **Test helpers**: extract shared config builders, keep test intent clear

## Verification

All 48 workspace tests pass:
- 14 unit tests (libtmax)
- 5 sandbox integration tests (libtmax)
- 12 protocol tests (tmax-protocol)
- 12 sandbox unit tests (tmax-sandbox)
- 4 web protocol tests (tmax-web)
- 5 web integration tests (tmax-web)

## Related Documentation

- [Phase 0 Code Review Solution](../security-issues/tmax-phase0-code-review.md) - Previous review findings
- [Feature Plan - Phase 2 Sandboxing](../../plans/2026-02-14-feat-tmax-terminal-multiplexer-plan.md)
- Completed todos: `todos/018-complete-*.md` through `todos/027-complete-*.md`
- Git commit: `8833fea` on `feat/phase2-sandboxing`
