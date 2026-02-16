---
status: done
priority: p3
issue_id: "018"
tags: [code-review, quality]
dependencies: []
---

# Improve Type Safety in Protocol

## Problem Statement

Several protocol fields use `String` where typed enums would be safer:
- `WalletBalance { wallet: String }` accepts any string, handler matches on "human"/"yolo"
- `message_type: String` in `InboxEntry` and `Event::NewMessage` uses `format!("{:?}", enum)` and string comparison in TUI
- `Response::Ok { data: Option<serde_json::Value> }` is type-erased

## Findings

- **Pattern Agent:** String-typed enums in protocol are fragile. Debug-format string comparison is error-prone.

## Proposed Solutions

### Replace Strings with Enums
- **Effort:** Small
- Add `WalletType` enum (Human, Yolo) with serde
- Add `MessageType` serde enum to protocol (reuse from mesh_pb or create new)
- Keep `serde_json::Value` for now (typed responses would be a larger refactor)

## Acceptance Criteria

- [x] `WalletBalance` uses typed `WalletType` enum
- [x] `InboxEntry` and `Event` use typed `MessageType`
- [x] No `format!("{:?}", ...)` for wire format values
- [x] TypeScript types updated to match

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by pattern review agent |
| 2026-02-16 | Completed | Added WalletType and MessageType enums to protocol, updated all Rust and TS consumers |
