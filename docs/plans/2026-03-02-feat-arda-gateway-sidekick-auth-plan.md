---
title: "feat: Integrate Arda Gateway for Sidekick inference with OAuth login"
type: feat
status: active
date: 2026-03-02
---

# Integrate Arda Gateway for Sidekick Inference with OAuth Login

## Overview

Replace the direct Anthropic API key flow in the TUI Sidekick with Arda Gateway-backed inference, gated behind an OAuth login. Users authenticate via Arda (Privy-based OAuth2), receive a `gw_sk_*` API key, and the Sidekick sends inference requests to Arda Gateway's Anthropic-compatible `/v1/messages` endpoint instead of hitting Anthropic directly.

This gives every agentbook user access to AI Sidekick through their Arda account, with built-in billing, rate limiting, and usage tracking — no raw Anthropic API key needed.

## Problem Statement / Motivation

Today the Sidekick requires users to bring their own Anthropic API key. This creates friction:

1. **Onboarding barrier** — new users must create an Anthropic account, generate a key, and paste it into the TUI before they can try Sidekick
2. **No usage visibility** — users have no dashboard, balance tracking, or spending controls
3. **No free tier path** — no way to offer trial credits or subsidized access
4. **Security concern** — raw Anthropic API keys stored as plaintext in `~/.local/state/agentbook/sidekick_anthropic_api_key`

Arda Gateway already solves all of these: OAuth login, per-project billing, balance tracking, rate limiting, and a web dashboard. The agentbook Sidekick just needs to be wired up to it.

## Proposed Solution

### High-Level Architecture

```
User → agentbook login (CLI)
         ↓
       Opens browser → Arda auth page (Privy login)
         ↓
       User authorizes → redirect to localhost callback
         ↓
       CLI exchanges auth code → gw_sk_* API key
         ↓
       Key stored in ~/.local/state/agentbook/arda_api_key (0600)
         ↓
User → TUI Sidekick enabled
         ↓
       pi-terminal-agent.mjs reads AGENTBOOK_GATEWAY_API_KEY env
         ↓
       Inference → Arda Gateway /v1/messages (Anthropic-compatible)
         ↓
       Arda Gateway → Vercel AI Gateway → Anthropic
```

### Key Design Decisions

1. **`agentbook login` as a standalone CLI command** — keeps the OAuth flow (localhost HTTP server, browser open) out of the TUI event loop. The TUI delegates by prompting "Run `agentbook login`" when no key is found.

2. **Static `gw_sk_*` API keys, not refresh tokens** — Arda Gateway's OAuth flow produces a static API key (no expiry, revocable from dashboard). This simplifies the client: no token refresh logic needed, just store and use.

3. **Arda Gateway URL + key passed via env vars** — `pi-terminal-agent.mjs` receives `AGENTBOOK_GATEWAY_URL` and `AGENTBOOK_GATEWAY_API_KEY` as env vars, overriding the Anthropic base URL. Minimal changes to the inference script.

4. **Backward-compatible migration** — detect key prefix to route: `gw_sk_*` → Arda Gateway, `sk-ant-*` → direct Anthropic. Existing users keep working until they choose to migrate.

## Technical Considerations

### Arda Gateway Integration Points

**OAuth App Registration (one-time setup):**
- Register agentbook as an OAuth app via `POST /api/v1/oauth/apps`
- Obtain `client_id` (`gw_app_*`) and `client_secret` (`gw_secret_*`)
- Configure redirect URI: `http://localhost:{port}/callback`
- Hardcode `client_id` in source (standard for open-source CLIs, like `gh auth login`)
- Store `client_secret` server-side only — the CLI uses PKCE instead

**OAuth Authorization Code Flow (per-user login):**
1. `GET /api/v1/oauth/authorize-info?client_id=gw_app_*&redirect_uri=http://localhost:{port}/callback` — validates app
2. User authenticates via Privy on the Arda web UI
3. `POST /api/v1/oauth/authorize` — user grants access (requires Privy JWT, handled by Arda web UI)
4. Redirect to `http://localhost:{port}/callback?code=gw_code_*`
5. `POST /api/v1/oauth/token` with `client_id`, `code`, `redirect_uri` — returns `gw_sk_*` API key

**Inference (per-request):**
- `POST {ARDA_GATEWAY_URL}/v1/messages` with `Authorization: Bearer gw_sk_*`
- Request/response format is Anthropic-compatible (same as current pi-terminal-agent.mjs)
- Response headers include `X-Gateway-Balance-Cents` and `X-Gateway-Balance-Warning: low`

### Files to Create/Modify

**New files:**
- `crates/agentbook-cli/src/cmd_login.rs` — OAuth login command implementation
- `crates/agentbook-cli/src/cmd_logout.rs` — Logout/key deletion command

**Modified files:**
- `crates/agentbook-cli/src/main.rs` — add `login` and `logout` subcommands
- `crates/agentbook-tui/src/automation.rs` — replace `save_anthropic_api_key`/`maybe_load_saved_anthropic_key` with Arda key loading, update env var propagation, update error detection for Arda-specific HTTP errors (402, 429)
- `crates/agentbook-tui/src/input.rs` — change `awaiting_api_key` UX from "paste key" to "run `agentbook login`"
- `crates/agentbook-tui/src/ui.rs` — update Sidekick pane auth prompts
- `agent/scripts/pi-terminal-agent.mjs` — add `AGENTBOOK_GATEWAY_URL`/`AGENTBOOK_GATEWAY_API_KEY` support, route inference to Arda Gateway when present, handle 402/429 error responses

### Security Considerations

- **PKCE required** for the OAuth flow to prevent auth code interception on localhost
- **Key storage**: `gw_sk_*` stored in `~/.local/state/agentbook/arda_api_key` with `0600` permissions (matches existing pattern). Unlike raw Anthropic keys, Arda keys have per-project spending limits and can be revoked from the dashboard.
- **Localhost callback**: bind to `127.0.0.1` only (not `0.0.0.0`), use a random high port, and shut down immediately after receiving the callback
- **No client_secret in source**: use PKCE (code_verifier/code_challenge) instead of embedding client_secret in the open-source CLI binary. This requires an update to Arda Gateway's OAuth token endpoint to accept PKCE in lieu of client_secret for public clients.

### Edge Cases

- **Browser doesn't open** (headless/SSH): print the auth URL to terminal, user visits manually on another device and pastes the resulting code
- **Port conflict**: try a few random ports (49152-65535 range), fail with clear error if all taken
- **User cancels OAuth**: localhost server times out after 120s, CLI exits with "Login cancelled"
- **Insufficient balance after login**: Sidekick shows "Arda account has no credits. Visit {dashboard_url} to add funds." in the chat pane
- **Key revoked mid-session**: 401 from Gateway triggers `awaiting_login = true` state, prompts re-login
- **Offline**: stored key loaded but inference fails with network error (same as today)

## System-Wide Impact

- **Interaction graph**: `agentbook login` → stores key file → TUI reads on startup via `maybe_load_arda_key()` → sets `AGENTBOOK_GATEWAY_API_KEY` + `AGENTBOOK_GATEWAY_URL` env vars → `pi-terminal-agent.mjs` reads env → sends inference to Arda Gateway → Gateway proxies to Anthropic
- **Error propagation**: Arda Gateway HTTP errors (401, 402, 429, 500) → pi-terminal-agent.mjs parses → returns structured `requires_login`/`billing_error`/`rate_limited` flags → TUI `apply_decision()` handles each case with specific UX
- **State lifecycle risks**: The only persistent state is the `arda_api_key` file. No risk of orphaned state. If the file is deleted, user simply re-runs `agentbook login`.
- **API surface parity**: Both the CLI and TUI consume the same stored key file. The standalone agent (`agent/src/index.ts`) could also read this key in the future.

## Acceptance Criteria

- [ ] `agentbook login` opens browser, completes OAuth, stores `gw_sk_*` key
- [ ] `agentbook logout` deletes the stored key
- [ ] TUI Sidekick uses Arda Gateway for inference when `arda_api_key` exists
- [ ] TUI prompts "Run `agentbook login` to enable Sidekick" when no key found
- [ ] Sidekick cannot be enabled without a valid Arda login (gating)
- [ ] Existing Anthropic API key users continue to work (backward compat)
- [ ] HTTP 401 from Gateway triggers re-login prompt
- [ ] HTTP 402 from Gateway shows "insufficient balance" with dashboard link
- [ ] HTTP 429 from Gateway shows rate limit message with retry-after
- [ ] Headless/SSH fallback: auth URL printed to terminal for manual flow
- [ ] Key file stored with `0600` permissions
- [ ] PKCE used for OAuth code exchange
- [ ] All existing Sidekick tests pass
- [ ] New tests for login/logout commands and key detection logic

## Success Metrics

- Users can go from `agentbook setup` to working Sidekick with just `agentbook login` (no API key hunting)
- Zero existing users broken on upgrade (backward compat with Anthropic keys)
- Arda Gateway balance/usage visible to users via their Arda dashboard

## Dependencies & Risks

**Dependencies:**
- Arda Gateway must support PKCE for public OAuth clients (or we use client_secret with a server-side proxy)
- Arda Gateway OAuth app must be registered with agentbook's `client_id`
- Arda Gateway must be deployed and accessible at a known URL

**Risks:**
- **Arda Gateway downtime** blocks all Sidekick usage (mitigation: fall back to direct Anthropic key if available)
- **PKCE support** may need to be added to Arda Gateway's `/api/v1/oauth/token` endpoint
- **Browser-based OAuth in CLI** is inherently fragile across environments (mitigation: manual code-paste fallback)

## Implementation Sketch

### Phase 1: CLI Login/Logout (`crates/agentbook-cli/`)

```rust
// crates/agentbook-cli/src/cmd_login.rs

use std::net::TcpListener;
use tokio::io::AsyncWriteExt;

const ARDA_CLIENT_ID: &str = "gw_app_agentbook";
const ARDA_AUTH_URL: &str = "https://app.arda.bot/oauth/authorize";
const ARDA_TOKEN_URL: &str = "https://gateway.arda.bot/api/v1/oauth/token";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(120);

pub async fn run_login() -> Result<()> {
    // 1. Generate PKCE code_verifier + code_challenge
    // 2. Bind localhost callback server on random port
    // 3. Build auth URL with client_id, redirect_uri, code_challenge
    // 4. Open browser (fall back to printing URL)
    // 5. Wait for callback with auth code (timeout 120s)
    // 6. Exchange code + code_verifier for gw_sk_* key
    // 7. Store key in state_dir/arda_api_key (0600)
    // 8. Print success message
}
```

### Phase 2: TUI Auth Gating (`crates/agentbook-tui/`)

```rust
// automation.rs — replace Anthropic key loading with Arda key

const ARDA_KEY_FILE: &str = "arda_api_key";
const ANTHROPIC_KEY_FILE: &str = "sidekick_anthropic_api_key"; // legacy

pub fn maybe_load_inference_key() -> Option<InferenceConfig> {
    // 1. Check for arda_api_key → InferenceConfig::Arda { key, gateway_url }
    // 2. Fall back to sidekick_anthropic_api_key → InferenceConfig::Anthropic { key }
    // 3. None if neither exists
}

pub enum InferenceConfig {
    Arda { key: String, gateway_url: String },
    Anthropic { key: String },
}
```

### Phase 3: Inference Routing (`agent/scripts/pi-terminal-agent.mjs`)

```javascript
// pi-terminal-agent.mjs — add Arda Gateway routing

function resolveInferenceConfig() {
  const gwKey = process.env.AGENTBOOK_GATEWAY_API_KEY;
  const gwUrl = process.env.AGENTBOOK_GATEWAY_URL;
  if (gwKey && gwUrl) {
    return { baseUrl: gwUrl, apiKey: gwKey, provider: "arda" };
  }
  // Legacy fallback
  const anthropicKey = process.env.AGENTBOOK_ANTHROPIC_API_KEY
    ?? process.env.ANTHROPIC_API_KEY;
  if (anthropicKey) {
    return { baseUrl: undefined, apiKey: anthropicKey, provider: "anthropic" };
  }
  return null;
}
```

## Sources & References

### Internal References
- Sidekick automation: `crates/agentbook-tui/src/automation.rs` (key storage lines 762-836, streaming lines 448-524)
- Sidekick state: `crates/agentbook-tui/src/app.rs` (AutoAgentState lines 73-91)
- Sidekick input: `crates/agentbook-tui/src/input.rs` (API key submission lines 1182-1206)
- Pi inference script: `agent/scripts/pi-terminal-agent.mjs` (resolveApiKey lines 37-44, error detection lines 783-800)
- CLI commands: `crates/agentbook-cli/src/main.rs`

### Arda Gateway References (~/development/arda-platform)
- OAuth endpoints: `POST /api/v1/oauth/authorize`, `POST /api/v1/oauth/token`
- Inference endpoint: `POST /v1/messages` (Anthropic-compatible)
- API key auth: `Authorization: Bearer gw_sk_*` or `x-api-key: gw_sk_*`
- Balance headers: `X-Gateway-Balance-Cents`, `X-Gateway-Balance-Warning`
- Error codes: 401 (auth), 402 (balance), 429 (rate limit)
- Gateway SDK: `@arda/gateway-sdk` — `ArdaGateway` class
