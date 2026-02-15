---
status: complete
priority: p1
issue_id: "029"
tags: [code-review, performance, tmax-client]
dependencies: []
---

# No Output Coalescing Causes Frame Drops During Bulk Output

## Problem Statement

The event loop renders after every single server Output event. During high-throughput scenarios (e.g., `cat large_file.txt`), hundreds of Output events arrive per second. Each triggers: vt100 process → screen.clone() → render_diff → stdout.flush() → status bar render. This causes visible sluggishness and frame drops since the client cannot keep up with the render-per-message approach.

## Findings

- **event_loop.rs:165-200**: Reads one message, renders immediately, loops back to select!
- **event_loop.rs:200**: `prev_screen = parser.screen().clone()` deep-clones full screen (rows*cols cells) per event
- **event_loop.rs:172-173**: Calls `terminal::size()` twice per output event (redundant syscalls)
- At ~1000 events/sec the client will fall behind, building backlog in socket buffer
- This is the single biggest performance concern identified

## Proposed Solutions

### Option A: Drain pending events before rendering (Recommended)
After receiving an Output event, drain all immediately-available Output events from the socket before rendering once.
- Pros: Simple, huge impact, reduces clone/render frequency proportionally
- Cons: Need a non-blocking try_read or poll mechanism
- Effort: Small-Medium
- Risk: Low

### Option B: Render throttle at 60fps
Use a timer to render at most every 16ms, accumulating all output in between.
- Pros: Predictable frame rate, decouples input from rendering
- Cons: Adds a timer to the select! loop, slightly more complex
- Effort: Medium
- Risk: Low

### Option C: Both (drain + throttle)
Combine draining with a max render rate.
- Pros: Best performance
- Cons: More complex
- Effort: Medium
- Risk: Low

## Technical Details

- **Affected files**: `crates/tmax-client/src/event_loop.rs`, `crates/tmax-client/src/connection.rs`
- **Components**: Event loop, ServerConnection

## Acceptance Criteria

- [ ] Client processes burst output without frame drops
- [ ] Screen clone happens at most once per render cycle (not per event)
- [ ] Terminal size is cached, not queried per event
- [ ] Existing tests still pass
