---
status: done
priority: p1
issue_id: "004"
tags: [code-review, architecture, oss]
dependencies: []
---

# Add CI Pipeline and OSS Essentials

## Problem Statement

No CI/CD pipeline exists. No automated quality gates. For a project handling private keys and crypto wallets, this is a significant gap. Additionally, missing SECURITY.md, CONTRIBUTING.md, and dependency auditing.

## Findings

- **Architecture Agent (HIGH):** No `.github/workflows/` directory. No CI at all.
- **Architecture Agent:** Missing SECURITY.md (critical for wallet-handling project), CONTRIBUTING.md, CHANGELOG.md.

## Proposed Solutions

### GitHub Actions CI
- **Effort:** Small
- `cargo test --workspace`, `cargo clippy -- -D warnings`, `cargo fmt --check`
- `cargo audit` for dependency vulnerabilities
- `cd agent && npm ci && npm run build` for TypeScript
- Add `SECURITY.md` with responsible disclosure process
- Add `CONTRIBUTING.md`

## Acceptance Criteria

- [ ] GitHub Actions workflow runs on push and PR
- [ ] Runs: cargo test, clippy, fmt check, cargo audit
- [ ] Builds TypeScript agent
- [ ] SECURITY.md exists with vulnerability reporting instructions
- [ ] CONTRIBUTING.md with development setup

## Work Log

| Date | Action | Notes |
|------|--------|-------|
| 2026-02-16 | Created | Found by architecture review agent |
