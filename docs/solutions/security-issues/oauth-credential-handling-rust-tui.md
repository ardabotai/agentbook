---
title: Safe OAuth Credential Handling in Rust CLI/TUI
category: security-issues
tags:
  - oauth-authentication
  - unsafe-env-vars
  - blocking-io-in-async
  - credential-caching
  - gateway-url-validation
  - filesystem-permissions
  - multi-threaded-safety
  - tui-performance
modules:
  - agentbook-cli
  - agentbook-tui
  - arda-gateway-integration
severity: high
issues_resolved: 8
date_resolved: 2026-03-03
rust_edition: "2024"
---

# Safe OAuth Credential Handling in Rust CLI/TUI

## Summary

During implementation and two rounds of multi-agent code review of the Arda Gateway OAuth feature, we identified and fixed 8 security and performance issues. The core patterns are reusable for any Rust application that handles credentials, spawns child processes, or runs a TUI event loop.

---

## Problem 1: Unsafe env var mutation in multi-threaded Rust

### Symptom
Rust 2024 edition requires `unsafe` for `std::env::set_var()` because it's not thread-safe. The TUI was using it to pass API credentials to child processes.

### Root Cause
`set_var` mutates global process state. In a multi-threaded tokio runtime, this is a data race.

### Solution
Store env vars in `Vec<(String, String)>` on app state, pass to child processes via `Command::env()`.

```rust
// WRONG: unsafe, affects all threads
unsafe { std::env::set_var("AGENTBOOK_GATEWAY_API_KEY", &key); }
let child = Command::new("node").arg("agent.mjs").spawn()?;

// RIGHT: safe, scoped to child process
let env_vars = load_inference_env_vars(); // returns Vec<(String, String)>
let mut cmd = Command::new("node");
cmd.arg("agent.mjs");
for (k, v) in &env_vars {
    cmd.env(k, v);
}
let child = cmd.spawn()?;
```

### Prevention
- **Rule:** Never call `std::env::set_var()` in multi-threaded code. Use `Command::env()` for child process IPC.
- **Detection:** Grep for `std::env::set_var` — any usage outside single-threaded setup is a bug.

---

## Problem 2: Blocking I/O in 60fps TUI render path

### Symptom
`has_arda_login()` called `fs::read_to_string()` from the draw function, performing a heap allocation and read syscall at 60fps.

### Root Cause
Filesystem I/O in the render hot path. Each call allocates a `String` and does a `read` syscall.

### Solution
Two-part fix: (a) cache result in `cached_has_arda: bool`, refresh only on login/logout; (b) use `fs::metadata()` (single stat syscall, no allocation).

```rust
// WRONG: heap allocation + read syscall per frame
fs::read_to_string(path).ok().is_some_and(|s| !s.trim().is_empty())

// RIGHT: single stat syscall, no allocation
fs::metadata(path).ok().is_some_and(|m| m.len() > 0)
```

### Prevention
- **Rule:** Never call `fs::` from draw/render functions. Load data before the loop or cache it.
- **Detection:** Trace the call stack from `draw()` — block any `fs::` calls in that path.

---

## Problem 3: Validation bypass on new code path

### Symptom
A new env-var fast-path for credential resolution skipped `is_valid_gateway_url()`. API keys could be sent over HTTP.

### Root Cause
When adding a shortcut code path, the developer applied validation only to the original disk-based path, not the new env-var path.

### Solution
Apply the same validation to all code paths.

```rust
let url = std::env::var("AGENTBOOK_GATEWAY_URL")
    .unwrap_or_else(|_| ARDA_DEFAULT_GATEWAY_URL.to_string());
if !is_valid_gateway_url(&url) {
    eprintln!("Warning: invalid AGENTBOOK_GATEWAY_URL (must be HTTPS), ignoring");
    // Fall through to next priority tier
} else {
    vars.push(("AGENTBOOK_GATEWAY_URL".to_string(), url));
    return vars;
}
```

### Prevention
- **Rule:** When adding a new code path (fast-path, shortcut), apply the same validation as the original path.
- **Detection:** In review, ask: "does this new branch skip any validation that other branches do?"

---

## Problem 4: Redundant disk I/O every tick

### Symptom
`load_inference_env_vars()` performs up to 4 filesystem reads, called every 6-second PI tick. Credentials change only on login/logout.

### Root Cause
No caching. The function was called unconditionally on every tick cycle.

### Solution
Cache with 30-second TTL. Force-refresh only on login/logout events.

```rust
const ENV_CACHE_TTL: Duration = Duration::from_secs(30);

fn refresh_inference_env_if_stale(app: &mut App) {
    let stale = app.auto_agent.last_env_load
        .map(|t| t.elapsed() >= ENV_CACHE_TTL)
        .unwrap_or(true);
    if stale {
        app.auto_agent.inference_env = load_inference_env_vars();
        app.auto_agent.last_env_load = Some(Instant::now());
    }
}
```

### Prevention
- **Rule:** Cache expensive operations with a TTL. Only force-refresh on explicit events.
- **Detection:** Ask: "Is this `load_*()` called every tick? Could it be called less often?"

---

## Problem 5: Directory permissions gap

### Symptom
`store_key()` used `create_dir_all` which respects umask (typically 0o755). Other users could list the directory containing key files.

### Root Cause
Using generic `create_dir_all` instead of the security-aware `ensure_state_dir()` helper that sets 0o700.

### Solution
```rust
// WRONG: default umask, potentially 0o755
std::fs::create_dir_all(state_dir)?;

// RIGHT: explicit 0o700
agentbook_mesh::state_dir::ensure_state_dir(state_dir)?;
```

### Prevention
- **Rule:** Never use bare `create_dir_all` for state/credential directories. Use `ensure_state_dir()`.
- **Detection:** Grep for `create_dir_all` outside of test fixtures — each instance needs review.

---

## Problem 6: Blocking async runtime

### Symptom
`wait_for_callback()` uses `std::thread::sleep(200ms)` in a polling loop, called from `async fn cmd_login()`. Blocks the Tokio runtime for up to 120 seconds.

### Root Cause
Synchronous blocking sleep inside an async function. Also, `set_nonblocking` called inside the loop when it only needs to be called once.

### Solution
Wrap in `spawn_blocking` and move `set_nonblocking` before the loop.

```rust
// WRONG: blocks the Tokio runtime thread
let code = wait_for_callback(listener, &state)?;

// RIGHT: runs on the blocking thread pool
listener.set_nonblocking(true)?; // once, before the loop
let code = tokio::task::spawn_blocking(move || wait_for_callback(listener, &state))
    .await
    .context("OAuth callback task panicked")??;
```

### Prevention
- **Rule:** Never call `std::thread::sleep` or blocking I/O inside `async fn`. Use `spawn_blocking`.
- **Detection:** Grep for `std::thread::sleep` inside async functions.

---

## General Patterns

### Credential IPC: Command::env() Pattern

For passing sensitive data to child processes:

```rust
// Isolated to child process, no global state mutation
Command::new("tool")
    .env("SECRET_KEY", secret_key)
    .spawn()?;
```

### Cache-with-TTL for Event Loops

```rust
struct CachedValue<T> {
    value: T,
    last_refreshed: Instant,
    ttl: Duration,
}

impl<T> CachedValue<T> {
    fn is_stale(&self) -> bool {
        self.last_refreshed.elapsed() > self.ttl
    }
    fn refresh(&mut self, value: T) {
        self.value = value;
        self.last_refreshed = Instant::now();
    }
}
```

### File Permission Conventions

| Resource | Permission | Rationale |
|----------|-----------|-----------|
| State directory | `0o700` | Only owning user can list contents |
| Unix socket | `0o600` | Only owning user can connect |
| API key files | `0o600` | Only owning user can read |
| Config (no secrets) | `0o644` | World-readable is fine |

---

## Review Checklist

When reviewing TUI, CLI, or async Rust code:

1. **Env vars?** Is `set_var` used? Use `Command::env()` instead.
2. **I/O in draw?** Does render call `fs::`? Cache before the loop.
3. **New code path?** Does fast-path skip validation? Apply same checks.
4. **Redundant I/O?** Is `load_*()` called every tick? Add TTL cache.
5. **Dir perms?** Is `create_dir_all` used for state? Use `ensure_state_dir`.
6. **Async blocking?** Is `std::thread::sleep` in async fn? Use `spawn_blocking`.

---

## Related Documentation

### Plan Documents
- Source-of-truth: `docs/plans/2026-02-16-pivot-agentbook.md`
- Arda Gateway auth plan: `docs/plans/2026-03-02-feat-arda-gateway-sidekick-auth-plan.md`

### Key Code Locations
- OAuth flow: `crates/agentbook-cli/src/login.rs`
- Inference routing: `crates/agentbook-tui/src/automation.rs`
- Shared constants: `crates/agentbook/src/gateway.rs`
- State dir utility: `agentbook_mesh::state_dir::ensure_state_dir()`

### Todos (020-040)
- 020-030: First review cycle (HTML injection, caching, --token flag, URL validation, dead code, security headers, unsafe set_var, agent support, auto-poll)
- 032-039: Second review cycle (env URL validation, dir perms, constant dedup, env caching, metadata check, Content-Length fix, spawn_blocking, --token warning)
- 031: Deferred — server-side token revocation
- 040: Deferred — protocol-level login/logout for agents
