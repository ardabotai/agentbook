---
status: done
priority: p2
issue_id: "014"
tags: [code-review, performance]
dependencies: []
---

# Cache gRPC Clients and Blockchain Providers

## Problem Statement

`handle_register_username` and `handle_lookup_username` create a new `HostServiceClient::connect()` for every request (TCP+HTTP/2 handshake each time). `read_contract` creates a new HTTP provider per call. Wallet balance fetches are sequential under lock.

## Findings

- **Performance Agent (OPT-1):** Per-request gRPC client creation wastes connection setup.
- **Performance Agent (OPT-7):** `read_contract` creates new provider per call.
- **Performance Agent (OPT-8):** Wallet lock held during sequential balance RPCs (can take seconds).

## Proposed Solutions

### Cache Clients in NodeState
- **Effort:** Small
- Store `HostServiceClient` per relay host (lazy init, reuse)
- Cache read-only provider for contract reads
- Parallelize balance fetches with `tokio::join!`

## Acceptance Criteria

- [x] gRPC clients reused across requests
- [x] Contract read provider cached
- [x] Balance fetches parallelized

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by performance review agent |
| 2026-02-16 | Implemented | Cached gRPC clients in NodeState HashMap, cached RootProvider via OnceLock, parallelized balance fetches with tokio::join! |
