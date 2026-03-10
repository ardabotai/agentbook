---
status: pending
priority: p1
issue_id: "059"
tags: [code-review, security, secrets, credential-exposure]
dependencies: []
---

# Add .env to .gitignore and Rotate Exposed OAuth Secrets

## Problem Statement

The `.env` file in the repo root contains live OAuth client secrets for both staging and production Arda Gateway environments. There is no `.env` entry in `.gitignore`, meaning a single `git add .` or `git add -A` would commit these secrets to the repository. The file is currently untracked (`?? .env` in git status).

## Findings

- **File:** `.env` — contains `STAGING_GATEWAY_CLIENT_SECRET=gw_secret_...` and `PRODUCTION_GATEWAY_CLIENT_SECRET=gw_secret_...`
- **File:** `.gitignore` — no `.env` rule exists
- **Source:** security-sentinel

## Proposed Solutions

### Solution A: Gitignore + rotate (recommended)
1. Add `.env`, `.env.*`, `*.env` to `.gitignore`
2. Rotate both staging and production client secrets immediately
3. Move secrets to a secrets manager (1Password, env-only injection)
- **Effort:** Small | **Risk:** None

## Acceptance Criteria

- [ ] `.gitignore` contains `.env` and common variants
- [ ] Both client secrets rotated on the Arda Gateway
- [ ] No secrets present in any tracked file

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-10 | Created from code review | Critical: secrets in working tree without gitignore protection |
