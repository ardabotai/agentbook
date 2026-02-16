---
status: done
priority: p3
issue_id: "019"
tags: [code-review, security]
dependencies: ["001"]
---

# Add TLS for Relay Connections

## Problem Statement

Relay connections use plain HTTP (gRPC without TLS). While E2E encryption (todo 001) protects message content, the relay connection metadata (node_id, connection timing, message routing) is visible to network observers.

## Findings

- **Security Agent:** gRPC connections to relay use plain HTTP. Even with E2E encryption, metadata is exposed.

## Proposed Solutions

### Add TLS Support to gRPC
- **Effort:** Medium
- Configure tonic with `tls-roots` feature
- Add `--tls-cert` and `--tls-key` options to host
- Default to TLS when connecting to non-localhost relays

## Acceptance Criteria

- [x] Relay host supports TLS
- [x] Client uses TLS by default for non-localhost
- [x] Self-signed cert workflow documented for self-hosting

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by security review agent |
| 2026-02-16 | Implemented | Added tls-ring + tls-webpki-roots to tonic, --tls-cert/--tls-key to host, auto https:// for non-localhost clients |
