---
status: done
priority: p2
issue_id: "013"
tags: [code-review, security]
dependencies: []
---

# Zeroize Key Material in Memory

## Problem Statement

The KEK (Key Encryption Key) lives as a plain `[u8; 32]` in `WalletConfig` for the entire daemon lifetime. `NodeIdentity::secret_key_bytes()` returns unzeroized copies. No `mlock` prevents swap exposure.

## Findings

- **Security Agent (HIGH-005):** KEK in process memory without zeroization. Memory dump/core file/swap could expose master secret.
- **Security Agent (LOW-004):** `secret_key_bytes()` returns copies that aren't wiped when dropped.

## Proposed Solutions

### Use `zeroize` and `secrecy` Crates
- **Effort:** Small
- Add `zeroize` dependency
- Use `secrecy::Secret<[u8; 32]>` or `Zeroizing<[u8; 32]>` for KEK
- Return `Zeroizing<[u8; 32]>` from `secret_key_bytes()`
- Consider `mlock` for key pages

## Acceptance Criteria

- [x] KEK uses `Zeroizing` or `Secret` wrapper
- [x] `secret_key_bytes()` returns zeroizable type
- [x] Key material zeroized on drop

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by security review agent |
| 2026-02-16 | Implemented | Added `zeroize` crate; wrapped KEK, recovery keys, and `secret_key_bytes()` in `Zeroizing<[u8; 32]>` |
