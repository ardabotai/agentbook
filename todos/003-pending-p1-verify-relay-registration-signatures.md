---
status: done
priority: p1
issue_id: "003"
tags: [code-review, security]
dependencies: []
---

# Verify Signatures at Relay Registration and Username Registration

## Problem Statement

The relay host accepts node registrations and username registrations without verifying signatures. An attacker can impersonate any node_id on the relay (intercepting their messages) or squat usernames for arbitrary node_ids.

## Findings

- **Security Agent (HIGH-002):** `RegisterFrame` contains `signature_b64` but relay never verifies it. Combined with CRITICAL-001 (no encryption), this enables full MITM.
- **Security Agent (HIGH-003):** `register_username` in router.rs ignores the `signature_b64` field entirely. Anyone can register usernames for any node_id.

## Proposed Solutions

### Option A: Add Signature Verification in Host
- **Effort:** Small
- **Risk:** Low
- Import `agentbook-crypto` verification functions into `agentbook-host`
- Verify `signature_b64` against `public_key_b64` for both relay registration and username registration
- Verify node_id matches the EVM address derived from the public key

## Acceptance Criteria

- [x] Relay verifies RegisterFrame signature before accepting connection
- [x] Username registration verifies signature before persisting
- [x] Invalid signatures return clear error messages
- [x] Tests cover both valid and invalid signature cases

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by security review agent |
| 2026-02-16 | Completed | Added verify_signature calls in relay and register_username handlers, 8 tests |
