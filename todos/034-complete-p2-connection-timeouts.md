---
status: complete
priority: p2
issue_id: "034"
tags: [code-review, security, robustness, tmax-client]
dependencies: []
---

# No Timeouts on Connection and Request/Response

## Problem Statement

Neither `connect()`, `send_request()`, nor `read_event()` have timeouts. A server that accepts but never responds locks the client in raw mode indefinitely. The user must kill the process to recover.

## Findings

- **connection.rs:18**: `UnixStream::connect` with no timeout
- **connection.rs:42-56**: `send_request` blocks waiting for response
- Previous Phase 0 review flagged similar blocking patterns

## Proposed Solutions

### Option A: Wrap with tokio::time::timeout (Recommended)
5s connect timeout, 10s request timeout.
- Effort: Small | Risk: Low

## Technical Details
- **Affected files**: `crates/tmax-client/src/connection.rs`

## Acceptance Criteria
- [ ] Connection times out after 5s with clear error
- [ ] Request/response times out after 10s
- [ ] Terminal restored properly on timeout
