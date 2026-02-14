---
status: pending
priority: p3
issue_id: "017"
tags: [code-review, architecture, quality]
dependencies: []
---

# Inconsistent Error Handling and Ad-hoc Error Mapping

## Problem Statement

Three error types (`TmaxError`, `BrokerError`, `anyhow`) with no systematic conversion. Error code mapping in `connection.rs` is ad-hoc -- e.g., `destroy_session` always maps to `SessionNotFound` even for non-not-found errors. Error messages leak internal paths to clients.

**Flagged by:** architecture-strategist, security-sentinel

## Findings

- `libtmax/error.rs` -- `TmaxError`
- `libtmax/broker.rs` -- `BrokerError` (separate, not unified)
- `tmax-server` and `tmax-cli` -- use `anyhow`, erasing structured errors
- `connection.rs` -- manual error-to-ErrorCode mapping in each match arm

## Proposed Solutions

### Option A: Implement `From<TmaxError> for (ErrorCode, String)` (Recommended)
Centralize error mapping, unify BrokerError into TmaxError, sanitize messages.

**Pros:** Type-safe mapping, no information leakage
**Cons:** Refactor across crates
**Effort:** Medium
**Risk:** Low

## Acceptance Criteria

- [ ] Single conversion from domain errors to protocol errors
- [ ] Error messages sanitized for clients
- [ ] BrokerError unified into TmaxError

## Work Log

| Date | Action |
|------|--------|
| 2026-02-14 | Created from code review |
