---
status: complete
priority: p1
issue_id: "028"
tags: [code-review, security, denial-of-service, tmax-client]
dependencies: []
---

# Unbounded Line Read Allows Memory Exhaustion

## Problem Statement

Both `send_request` and `read_event` in `connection.rs` use `BufReader::read_line` with no upper bound on line length. A compromised server or man-in-the-middle on the socket could send an infinitely long line without a newline, causing the client to allocate memory until OOM-killed. This is the most impactful security finding since terminal stays in raw mode during the hang.

## Findings

- **connection.rs:48-49**: `send_request` allocates `String::new()` then calls `read_line` with no limit
- **connection.rs:61-62**: `read_event` has the same unbounded read pattern
- Previous Phase 0 review (docs/solutions/security-issues/tmax-phase0-code-review.md) flagged similar patterns
- While the server is trusted (same-user process), a socket hijack or compromised server could exploit this

## Proposed Solutions

### Option A: Bounded read wrapper (Recommended)
Add a `MAX_LINE_LENGTH` constant (e.g., 16 MiB) and use `fill_buf()` to check length before consuming.
- Pros: Precise control, no dependencies
- Cons: More code than read_line
- Effort: Small
- Risk: Low

### Option B: Read into fixed-capacity buffer
Use `Vec::with_capacity` and `take()` adapter to limit reads.
- Pros: Simple
- Cons: Less flexible
- Effort: Small
- Risk: Low

## Technical Details

- **Affected files**: `crates/tmax-client/src/connection.rs`
- **Components**: ServerConnection

## Acceptance Criteria

- [ ] `read_event` and `send_request` reject messages over MAX_LINE_LENGTH
- [ ] Client produces a clear error message when limit exceeded
- [ ] Existing tests still pass
- [ ] Add test for oversized message rejection
