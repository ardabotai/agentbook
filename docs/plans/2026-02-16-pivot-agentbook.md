# Plan: Pivot tmax to agentbook

## Context

**agentbook** is an AI-powered encrypted messaging network. Each person runs a node daemon and interacts through a TUI where their AI agent helps them draft, refine, and send messages. Recipients' agents can act on incoming messages autonomously or present them to their human.

It's a social network where every user has an AI assistant as their interface.

## Social Model

**Twitter-style follow graph with encrypted messaging:**

- **Follow** (one-way) — you see their encrypted feed posts. Posts are encrypted per-follower (symmetric content key wrapped with each follower's pubkey via ECDH).
- **Mutual follow** — unlocks DMs. End-to-end encrypted via ECDH + ChaCha20-Poly1305.
- **Block** — cuts off everything (unfollow + reject all messages).
- **Relay is zero-knowledge** — only forwards encrypted blobs. Cannot read DMs or feed posts.

**Username directory on relay host:**

- Nodes register a username (e.g. `@chris`) on the relay host, signed by their private key.
- Lookup: resolve `@username` to node ID + public key.
- Follow by username: `agentbook follow @chris`.

## Agent Harness

**pi-ai** (`@mariozechner/pi-ai`) from the pi-mono toolkit:

- TypeScript unified LLM API — Anthropic, OpenAI, Google, Groq, xAI, Ollama, vLLM, etc.
- Simple API: `stream(model, context)` / `complete(model, context)` with tool calling
- Context serialization as plain JSON for persistence and handoff
- TypeBox schemas for tool definitions with automatic validation

**Hybrid architecture:**

- **Rust**: `agentbook-node` daemon, crypto, mesh, relay — handles identity, encryption, transport
- **TypeScript (pi-ai)**: agent process — drafts messages, summarizes inbox, suggests responses, calls tools against the node's Unix socket API
- **TUI**: Rust (ratatui) — connects to both the node daemon and the TypeScript agent process

## What we keep from tmax

| Current crate | Rename to | Why |
|---|---|---|
| `tmax-crypto` | `agentbook-crypto` | ECDH, ECDSA, ChaCha20-Poly1305 for identity + encrypted messaging |
| `tmax-mesh` | `agentbook-mesh` | Identity, friends→followers, invite, inbox, relay transport |
| `tmax-mesh-proto` | `agentbook-proto` | Protobuf defs for PeerService + HostService |
| `tmax-host` | `agentbook-host` | Relay/rendezvous server + username directory |
| `tmax-mesh-tests` | `agentbook-tests` | E2E mesh tests |

## What we delete

All terminal-related crates:
- `libtmax`, `tmax-protocol`, `tmax-sandbox`, `tmax-sandbox-runner`, `tmax-git`
- `tmax-local`, `tmax-cli`, `tmax-web`, `tmax-client`, `tmax-agent-sdk`
- `tmax-node`, `tmax-node-proto`

Old docs: `docs/plans/`, `docs/brainstorms/`, `docs/releases/`, `docs/research/`, `docs/operations/`
Old ops: `ops/`, `scripts/`, `AGENT.md`

## What we build new

### 1. `agentbook` — shared lib crate

Protocol types for the Unix socket API between CLI/TUI and the node daemon:
- Request/response enums for: identity, follow/unfollow, followers/following lists, block, send DM, send feed post, inbox, register username, lookup username
- `NodeId`, `Username`, `FollowRecord`, `Message`, `FeedPost` types
- Client helper for connecting to the node's Unix socket

### 2. `agentbook-node` — the daemon

Background daemon that:
- Manages cryptographic identity (secp256k1 keypair)
- Maintains follow graph (follow, unfollow, block)
- Connects to relay hosts for NAT traversal
- Registers username on relay host directory
- Receives and stores encrypted messages (DMs + feed posts) in inbox
- Encrypts outbound feed posts per-follower, DMs per-recipient
- Exposes a Unix socket API for TUI/CLI
- Handles gRPC PeerService for node-to-node communication

### 3. `agentbook-cli` — headless CLI (binary: `agentbook`)

```
agentbook up [--foreground]              Start the node daemon
agentbook down                           Stop the node daemon
agentbook identity                       Show node ID, public key, username
agentbook register <username>            Register username on relay
agentbook lookup <username>              Resolve username to node ID

agentbook follow <@username|node-id>     Follow a node
agentbook unfollow <@username|node-id>   Unfollow
agentbook block <@username|node-id>      Block
agentbook followers                      List followers
agentbook following                      List following

agentbook send <@username|node-id> <msg> Send a DM (requires mutual follow)
agentbook post <message>                 Post to feed (sent to all followers)
agentbook inbox [--unread]               List inbox (DMs + feed)
agentbook inbox ack <message-id>         Ack a message

agentbook health                         Health check
```

### 4. `agentbook-tui` — TUI chat client (ratatui)

Two main views:

**Feed view** — timeline of encrypted posts from everyone you follow:
- Posts decrypted and displayed chronologically
- Agent can summarize, filter, highlight
- Reply, repost, or start DM from any feed item

**DM view** — 1:1 encrypted conversations:
- Left sidebar: mutual follows with unread counts
- Main pane: conversation thread
- Bottom: input area
- Chat with your agent to draft messages

**Common:**
- Tab/keybind to switch Feed ↔ DMs
- Agent always available — knows whether you're composing, asking a question, or giving an instruction
- **Agent never sends without human approval** — drafts and suggests, human confirms
- Agent summarizes incoming, suggests responses

### 5. `agentbook-agent` — TypeScript agent process

TypeScript process using pi-ai that:
- Connects to the node daemon via Unix socket
- Has tools to: read inbox, draft messages, send messages (with human approval gate), search followers, summarize conversations
- Runs as a sidecar process spawned by the TUI or independently
- Supports any LLM provider via pi-ai (Anthropic, OpenAI, Google, local models)

## Mesh layer changes

### Replace friends with follow graph
- `FollowRecord`: `{ node_id, username, relay_hints, followed_at }`
- `BlockRecord`: `{ node_id, blocked_at }`
- Mutual follow detection: both sides have a `FollowRecord` for each other
- DM gating: only allow DMs between mutual follows

### Remove TrustTier
- Friends are binary: you follow them or you don't
- Ingress validation: signature check → follower/mutual-follow check → rate limit

### Add username directory to relay host
- `RegisterUsername { username, node_id, signature }` — claim a username
- `LookupUsername { username }` → `{ node_id, public_key }`
- Username uniqueness enforced by relay host
- Signed by node's private key to prevent squatting

### Feed post encryption
- Generate symmetric content key per post
- Encrypt post body with content key (ChaCha20-Poly1305)
- For each follower: wrap content key with ECDH shared secret
- Relay stores encrypted blob + per-follower key wraps

### DM encryption
- ECDH between sender and recipient public keys
- Derive shared secret → ChaCha20-Poly1305 encrypt message
- Relay only sees encrypted blob

## File structure

```
crates/
  agentbook/              # shared types, Unix socket protocol, client helpers
  agentbook-cli/          # headless CLI binary
  agentbook-crypto/       # crypto primitives (from tmax-crypto)
  agentbook-host/         # relay server + username directory (from tmax-host)
  agentbook-mesh/         # mesh lib: identity, follow graph, inbox, transport (from tmax-mesh)
  agentbook-node/         # daemon: identity + follows + relay + inbox + encryption
  agentbook-proto/        # protobuf defs (from tmax-mesh-proto)
  agentbook-tests/        # E2E tests (from tmax-mesh-tests)
  agentbook-tui/          # TUI chat client (ratatui)
agent/                    # TypeScript agent process (pi-ai)
  package.json
  tsconfig.json
  src/
    index.ts              # agent entry point
    tools/                # tool definitions for node interaction
    context/              # conversation context management
```

## Execution order

### Phase 1: Clean slate
1. Delete terminal crates (libtmax, tmax-protocol, tmax-sandbox, tmax-sandbox-runner, tmax-git, tmax-local, tmax-cli, tmax-web, tmax-client, tmax-agent-sdk, tmax-node, tmax-node-proto)
2. Delete old docs (docs/plans, docs/brainstorms, docs/releases, docs/research, docs/operations, AGENT.md, ops/, scripts/)
3. Rename kept crates (tmax-crypto → agentbook-crypto, tmax-mesh → agentbook-mesh, tmax-mesh-proto → agentbook-proto, tmax-host → agentbook-host, tmax-mesh-tests → agentbook-tests)
4. Update root Cargo.toml — remove unused workspace deps, update crate refs
5. Remove TrustTier from mesh layer, replace friends with follow graph

### Phase 2: Core protocol + daemon
6. Create `agentbook` lib crate — Unix socket protocol types
7. Build `agentbook-node` — daemon with Unix socket API + gRPC peer service + encryption
8. Add username directory to `agentbook-host`

### Phase 3: Clients
9. Build `agentbook-cli` — headless CLI
10. Build `agentbook-tui` — chat TUI with feed + DM views

### Phase 4: Agent
11. ~~Scaffold `agent/` TypeScript project with pi-ai~~ DONE
12. ~~Implement agent tools for node interaction~~ DONE
13. ~~Wire agent into TUI as sidecar process~~ DONE

### Phase 5: Polish
14. Write README.md
15. Update CLAUDE.md
16. Write agent skill (skills/agentbook/SKILL.md)

## Verification

1. `cargo clippy --workspace --all-targets -- -D warnings` — clean
2. `cargo test --workspace` — all pass
3. Smoke: `agentbook up` → `agentbook identity` → `agentbook register chris` → `agentbook follow @alice` → `agentbook send @alice "hello"` → `agentbook inbox` → `agentbook down`
4. E2E: two-node messaging test passes via relay
5. TUI launches and connects to running node
6. Agent process starts and responds to prompts
