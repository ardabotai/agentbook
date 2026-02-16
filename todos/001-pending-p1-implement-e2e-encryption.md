---
status: done
priority: p1
issue_id: "001"
tags: [code-review, security, architecture]
dependencies: []
---

# Implement End-to-End Encryption for Messages

## Problem Statement

Messages (DMs and feed posts) are transmitted as base64-encoded **plaintext** through the relay, despite the architecture and README claiming "every message is end-to-end encrypted via ECDH + ChaCha20-Poly1305." The relay can read all message content. This is the single most critical gap for production readiness.

The cryptographic primitives (`derive_pairwise_key`, `encrypt_with_key`, `decrypt_with_key`) already exist in `agentbook-crypto` but are not wired into the handler.

## Findings

- **Security Agent (CRITICAL-001):** Messages in `handle_send_dm` (handler.rs:343), `handle_post_feed` (handler.rs:377-393), and `process_inbound` (handler.rs:869-894) all use plain base64 instead of ECDH+ChaCha20.
- **Architecture Agent (CRITICAL):** Six TODO comments mark where encryption should be. The crypto module is complete but unused.
- **Pattern Agent (HIGH):** The constraint "All messages must be encrypted before leaving the node" from CLAUDE.md is violated.

## Proposed Solutions

### Option A: Wire Up Existing Crypto Primitives
- **Effort:** Medium
- **Risk:** Low (primitives are tested)
- Use `derive_pairwise_key(my_secret, peer_public)` for DM shared key
- Call `encrypt_with_key` with fresh random nonce for each message
- For feed posts: generate random content key, encrypt body, wrap key per-follower
- On receive: `decrypt_with_key` in `process_inbound`

### Option B: Implement Noise Protocol Framework
- **Effort:** Large
- **Risk:** Medium
- Replace custom ECDH with Noise XX or IK handshake
- Adds forward secrecy
- More complex but stronger security guarantees

## Recommended Action

Option A -- wire up existing primitives first, iterate to Noise later if needed.

## Technical Details

**Affected files:**
- `crates/agentbook-node/src/handler.rs` (handle_send_dm, handle_post_feed, process_inbound)
- `crates/agentbook-crypto/src/crypto.rs` (existing: derive_pairwise_key, encrypt_with_key, decrypt_with_key)

## Acceptance Criteria

- [ ] DMs are encrypted with ECDH shared key + ChaCha20-Poly1305
- [ ] Feed posts use per-follower content key wrapping
- [ ] `process_inbound` decrypts and verifies signatures
- [ ] Relay sees only ciphertext (verify by inspection)
- [ ] Nonce is random and non-empty in every envelope
- [ ] Round-trip integration test: send encrypted DM, verify decryption

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by security, architecture, and pattern review agents |

## Resources

- handler.rs TODO comments at lines 343, 377, 869, 884
- `agentbook-crypto/src/crypto.rs` for existing primitives
