---
title: "refactor: unify tmax-node client API on Unix socket"
type: refactor
status: complete
date: 2026-02-15
---

# refactor: unify tmax-node client API on Unix socket

## Goal

Replace tmax-node's gRPC client API (`--listen`) with a Unix socket using the same JSON-lines protocol as tmax-local. Keep gRPC only for the peer-to-peer service (`--peer-listen`), which genuinely needs TCP for cross-machine communication.

After this change, tmax-node becomes "tmax-local + mesh state + peer gRPC service" rather than a parallel gRPC reimplementation of the same session engine.

## Problem

Today tmax-node serves a gRPC `TmaxNode` service with 32 RPC methods. Of those, 23 are functionally identical to tmax-local's Unix socket handlers — they just translate between protobuf types and the same `SessionManager` calls. This creates:

1. **~700 lines of proto↔protocol conversion boilerplate** that must stay in sync with tmax-local
2. **Two client interfaces** for the same operations (JSON-lines for tmax-local, gRPC for tmax-node)
3. **CLI can't talk to nodes** without a separate gRPC client path (currently it only speaks JSON-lines)
4. **Double the surface area** to test and maintain for equivalent functionality

## Current state

### tmax-node `--listen` (gRPC TmaxNode service, 32 methods)

**Session management (12 methods)** — identical to tmax-local:
- SessionCreate, SessionDestroy, SessionList, SessionTree, SessionInfo
- Attach, Detach, SendInput, Resize
- MarkerInsert, MarkerList, Subscribe

**Messaging & workflows (11 methods)** — identical to tmax-local:
- MessageSend, MessageList, MessageAck, MessageUnreadCount, WalletInfo
- WorkflowCreate, WorkflowJoin, WorkflowLeave, WorkflowList
- TaskCreate, TaskList, TaskClaim, TaskSetStatus

**Mesh-specific (9 methods)** — NOT in tmax-local:
- NodeInfo
- InviteCreate, InviteAccept
- FriendsList, FriendsBlock, FriendsUnblock, FriendsRemove, FriendsSetTrust
- NodeInboxList, NodeInboxAck, NodeSendRemote

### tmax-node `--peer-listen` (gRPC PeerService, 1 method)

- SendMessage (accepts encrypted Envelope, returns Ack)

This stays on gRPC — it's the network-facing peer-to-peer service.

## Proposed architecture

```
tmax-node
  ├── Unix socket (JSON-lines)     ← local client API (same protocol as tmax-local)
  │     23 existing Request variants (session, messaging, workflows, tasks)
  │     + 9 new mesh Request variants (node info, friends, inbox, remote send)
  │
  └── gRPC TCP (PeerService only)  ← network-facing peer-to-peer envelope delivery
        1 method: SendMessage(Envelope) → Ack
```

CLI usage becomes identical for both tmax-local and tmax-node:
```bash
tmax list                              # works against tmax-local OR tmax-node
tmax msg send --to <session> "hello"   # same protocol, same socket discovery
tmax node info                         # new: mesh-specific commands (only on tmax-node)
tmax friends list                      # new: mesh-specific commands
```

## Implementation plan

### Step 1: Add mesh request/response variants to tmax-protocol

Add 9 new `Request` variants and corresponding response types:

```rust
// In tmax-protocol/src/lib.rs, extend Request enum:

NodeInfo,

InviteCreate {
    relay_hosts: Vec<String>,
    scopes: Vec<String>,
    ttl_ms: u64,
},
InviteAccept {
    token: String,
},

FriendsList,
FriendsBlock { node_id: String },
FriendsUnblock { node_id: String },
FriendsRemove { node_id: String },
FriendsSetTrust {
    node_id: String,
    trust_tier: TrustTier,
},

NodeInboxList {
    #[serde(default)]
    unread_only: bool,
    limit: Option<usize>,
},
NodeInboxAck {
    message_id: String,
},
NodeSendRemote {
    to_node_id: String,
    topic: Option<String>,
    body: String,
    #[serde(default)]
    encrypt: bool,
    invite_token: Option<String>,
    message_type: MeshMessageType,
},
```

Also add supporting types: `TrustTier`, `MeshMessageType`, `NodeInboxMessage`, `FriendRecord`, `NodeInfoResponse`.

This is backward-compatible — tmax-local will return an error for unknown variants, existing clients don't send them.

**Tests:** Protocol serialization round-trip for all new variants.

### Step 2: Extract tmax-local's connection handler into a reusable module

Currently `tmax-local/src/main.rs` has `handle_connection()` and `handle_request()` as private functions. Extract the request dispatch logic so tmax-node can reuse it.

Options:
- **A) Move to libtmax** — extract a `ConnectionHandler` that takes a `SessionManager` + write channel and dispatches `Request` → `Response`. Both tmax-local and tmax-node use it.
- **B) Keep in tmax-local, make pub** — tmax-node depends on tmax-local as a library. Simpler but creates a dependency from tmax-node → tmax-local.
- **C) Duplicate** — copy the dispatch logic into tmax-node. Least coupling but code duplication.

**Recommended: Option A.** The dispatch logic is pure business logic (Request → SessionManager call → Response) with no server-specific state. It belongs in libtmax.

This involves:
- Extract `handle_request()` into `libtmax::request_handler`
- Pass mesh-specific requests through as an extension trait or callback
- tmax-local calls it for core requests, returns error for mesh requests
- tmax-node calls it for core requests, handles mesh requests itself

**Tests:** Existing tmax-local integration tests should pass unchanged.

### Step 3: Add Unix socket server to tmax-node

Wire up tmax-node to listen on a Unix socket with the same accept loop pattern as tmax-local:

- Accept connections, enforce peer UID check
- Read JSON-lines, dispatch through shared handler
- For mesh-specific requests (NodeInfo, FriendsList, etc.), handle locally using NodeIdentity/FriendsStore/NodeInbox/MeshTransport
- For Subscribe, use the same mpsc broadcast pattern as tmax-local

New flags:
```
tmax-node [--socket PATH]              # Unix socket for local clients (default: auto-discover)
          [--peer-listen ADDR]         # gRPC for peer-to-peer (optional, TCP)
          [--relay-host ADDR]...       # relay hosts for NAT traversal
          [--config PATH]
          [--comms-policy POLICY]
```

`--listen` is removed. `--socket` replaces it.

**Tests:** Port existing tmax-node gRPC integration tests to Unix socket.

### Step 4: Add mesh CLI commands to tmax-cli

Add new subcommand groups:

```bash
tmax node info                         # NodeInfo
tmax invite create --ttl-ms 3600000    # InviteCreate
tmax invite accept <token>             # InviteAccept
tmax friends list                      # FriendsList
tmax friends block <node-id>           # FriendsBlock
tmax friends unblock <node-id>         # FriendsUnblock
tmax friends remove <node-id>          # FriendsRemove
tmax friends trust <node-id> <tier>    # FriendsSetTrust
tmax inbox list [--unread]             # NodeInboxList
tmax inbox ack <message-id>            # NodeInboxAck
tmax remote send <node-id> "body"      # NodeSendRemote
```

These commands use the same Unix socket transport as all other tmax commands. They'll return an error if the server is tmax-local (which doesn't support mesh operations).

**Tests:** CLI argument parsing + mock-server integration tests.

### Step 5: Remove gRPC from tmax-node client API

- Delete the `TmaxNode` gRPC service impl (all 32 method handlers)
- Delete proto↔protocol conversion functions (~700 lines)
- Remove `TmaxNode` from `tmax-node-proto/proto/tmax_node.proto` (keep only PeerService import from tmax-mesh-proto)
- If tmax-node-proto only re-exports tmax-mesh-proto at that point, consider deleting tmax-node-proto entirely and having tmax-node depend directly on tmax-mesh-proto
- Remove tonic server setup for `--listen`
- Clean up Cargo.toml dependencies (tonic/prost may reduce to only what PeerService needs)

**Tests:** All existing tests rewritten against Unix socket in Step 3.

### Step 6: Update docs

- README.md — update mesh networking section (remove `--listen`, show `--peer-listen` only)
- CLAUDE.md — update architecture diagram (tmax-node now uses Unix socket)
- AGENT.md — note unified CLI interface
- docs/plans/2026-02-15-feat-tmax-mesh-relay-rendezvous.md — update current state section

## Crate impact summary

| Crate | Change | Risk |
|---|---|---|
| `tmax-protocol` | +9 Request variants, +5 types | Low — additive, backward-compatible |
| `libtmax` | Extract request handler module | Medium — refactor, existing tests must pass |
| `tmax-local` | Use extracted handler | Low — behavior unchanged |
| `tmax-node` | Replace gRPC with Unix socket, delete proto conversions | High — major rewrite, but simpler result |
| `tmax-node-proto` | Remove TmaxNode service (or delete crate) | Medium — breaking for any gRPC clients |
| `tmax-cli` | Add mesh subcommands | Low — additive |
| `tmax-mesh-proto` | No change (PeerService stays) | None |

## What we gain

- **One protocol** — CLI talks to tmax-local and tmax-node the same way
- **~700 fewer lines** of proto conversion boilerplate
- **Same security model** — Unix socket peer UID checks, `0600` permissions
- **No port allocation** — no `--listen` port conflicts
- **Simpler testing** — Unix socket tests are simpler than gRPC
- **CLI mesh commands** — `tmax friends list`, `tmax inbox list`, etc. work out of the box

## What we lose

- **gRPC client API** — any external tools using gRPC to talk to tmax-node will break. As of today there are no known external gRPC clients (the CLI doesn't use it).
- **Proto type safety** — gRPC proto gives compile-time type checking for request/response shapes. JSON-lines relies on serde, which is runtime. Mitigated by existing test coverage patterns.

## Open questions

- [ ] Should tmax-node-proto be deleted entirely if it only contains PeerService (which is already defined in tmax-mesh-proto)?
- [ ] Should the extracted request handler support middleware/hooks for mesh-specific pre/post processing (e.g., auto-routing messages to remote nodes)?
- [ ] Should tmax-node embed tmax-local as a library dependency, or should both independently depend on the extracted handler in libtmax?
