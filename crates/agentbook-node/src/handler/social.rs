use super::{NodeState, error_response, now_ms, ok_response};
use agentbook::protocol::{FollowInfo, HealthStatus, IdentityInfo, Response, SyncResult};
use agentbook_mesh::follow::FollowRecord;
use agentbook_proto::host::v1 as host_pb;
use std::sync::Arc;

/// Resolved target info from a `@username` or raw node_id.
struct ResolvedTarget {
    node_id: String,
    public_key_b64: String,
    username: Option<String>,
}

/// Resolve a target that may be `@username` or a raw node_id.
/// If it starts with `@`, performs a relay lookup to get node_id + pubkey.
/// Otherwise returns the raw target with no pubkey (caller can look it up locally).
async fn resolve_target(state: &Arc<NodeState>, target: &str) -> Result<ResolvedTarget, Response> {
    if let Some(username) = target.strip_prefix('@') {
        if state.relay_hosts.is_empty() {
            return Err(error_response(
                "no_relay",
                "not connected to any relay — cannot resolve @username",
            ));
        }

        for host in &state.relay_hosts {
            let mut client = match state.get_grpc_client(host).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(host = %host, err = %e, "failed to connect for username lookup");
                    continue;
                }
            };

            match client
                .lookup_username(host_pb::LookupUsernameRequest {
                    username: username.to_string(),
                })
                .await
            {
                Ok(resp) => {
                    let r = resp.into_inner();
                    if r.found {
                        return Ok(ResolvedTarget {
                            node_id: r.node_id,
                            public_key_b64: r.public_key_b64,
                            username: Some(username.to_lowercase()),
                        });
                    }
                    return Err(error_response(
                        "not_found",
                        &format!("username @{} not found", username.to_lowercase()),
                    ));
                }
                Err(e) => {
                    tracing::warn!(host = %host, err = %e, "lookup_username RPC failed");
                    continue;
                }
            }
        }

        Err(error_response(
            "relay_unavailable",
            "could not reach any relay for username resolution",
        ))
    } else {
        Ok(ResolvedTarget {
            node_id: target.to_string(),
            public_key_b64: String::new(),
            username: None,
        })
    }
}

/// Notify the relay about a follow relationship (best-effort, non-blocking).
async fn notify_relay_follow(state: &Arc<NodeState>, followed_node_id: &str) {
    let sig = match state.identity.sign(state.identity.node_id.as_bytes()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(err = %e, "failed to sign for NotifyFollow");
            return;
        }
    };

    for host in &state.relay_hosts {
        let mut client = match state.get_grpc_client(host).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "failed to connect for NotifyFollow");
                continue;
            }
        };

        match client
            .notify_follow(host_pb::NotifyFollowRequest {
                follower_node_id: state.identity.node_id.clone(),
                followed_node_id: followed_node_id.to_string(),
                signature_b64: sig.clone(),
            })
            .await
        {
            Ok(resp) => {
                let r = resp.into_inner();
                if !r.success {
                    tracing::warn!(err = ?r.error, "relay NotifyFollow failed");
                }
                return;
            }
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "NotifyFollow RPC failed");
                continue;
            }
        }
    }
}

/// Notify the relay about an unfollow (best-effort, non-blocking).
async fn notify_relay_unfollow(state: &Arc<NodeState>, followed_node_id: &str) {
    let sig = match state.identity.sign(state.identity.node_id.as_bytes()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(err = %e, "failed to sign for NotifyUnfollow");
            return;
        }
    };

    for host in &state.relay_hosts {
        let mut client = match state.get_grpc_client(host).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "failed to connect for NotifyUnfollow");
                continue;
            }
        };

        match client
            .notify_unfollow(host_pb::NotifyUnfollowRequest {
                follower_node_id: state.identity.node_id.clone(),
                followed_node_id: followed_node_id.to_string(),
                signature_b64: sig.clone(),
            })
            .await
        {
            Ok(resp) => {
                let r = resp.into_inner();
                if !r.success {
                    tracing::warn!(err = ?r.error, "relay NotifyUnfollow failed");
                }
                return;
            }
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "NotifyUnfollow RPC failed");
                continue;
            }
        }
    }
}

/// Fetch followers from the relay via GetFollowers RPC.
pub(crate) async fn fetch_followers_from_relay(
    state: &Arc<NodeState>,
    node_id: &str,
) -> Result<Vec<host_pb::FollowEntry>, String> {
    for host in &state.relay_hosts {
        let mut client = match state.get_grpc_client(host).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "failed to connect for GetFollowers");
                continue;
            }
        };

        match client
            .get_followers(host_pb::GetFollowersRequest {
                node_id: node_id.to_string(),
            })
            .await
        {
            Ok(resp) => return Ok(resp.into_inner().followers),
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "GetFollowers RPC failed");
                continue;
            }
        }
    }

    Err("could not reach any relay for GetFollowers".to_string())
}

pub async fn handle_identity(state: &Arc<NodeState>) -> Response {
    let username = state.username.lock().await.clone();
    let info = IdentityInfo {
        node_id: state.identity.node_id.clone(),
        public_key_b64: state.identity.public_key_b64.clone(),
        username,
    };
    ok_response(Some(serde_json::to_value(info).unwrap()))
}

pub async fn handle_health(state: &Arc<NodeState>) -> Response {
    let follow_store = state.follow_store.lock().await;
    let inbox = state.inbox.lock().await;
    let status = HealthStatus {
        healthy: true,
        relay_connected: state.transport.is_some(),
        following_count: follow_store.following().len(),
        unread_count: inbox.unread_count(),
    };
    ok_response(Some(serde_json::to_value(status).unwrap()))
}

pub async fn handle_follow(state: &Arc<NodeState>, target: &str) -> Response {
    // Resolve @username → node_id + pubkey
    let resolved = match resolve_target(state, target).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let record = agentbook_mesh::follow::FollowRecord {
        node_id: resolved.node_id.clone(),
        public_key_b64: resolved.public_key_b64,
        username: resolved.username,
        relay_hints: vec![],
        followed_at_ms: now_ms(),
    };

    {
        let mut follow_store = state.follow_store.lock().await;
        if let Err(e) = follow_store.follow(record) {
            return error_response("follow_failed", &e.to_string());
        }
    }

    // Notify relay (best-effort)
    notify_relay_follow(state, &resolved.node_id).await;

    ok_response(None)
}

pub async fn handle_unfollow(state: &Arc<NodeState>, target: &str) -> Response {
    // Resolve @username → node_id
    let resolved = match resolve_target(state, target).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    {
        let mut follow_store = state.follow_store.lock().await;
        if let Err(e) = follow_store.unfollow(&resolved.node_id) {
            // Also try the raw target string — handles stale entries stored with
            // a username as the node_id (e.g. "@agent0" instead of "0x…").
            if target != resolved.node_id {
                if follow_store.unfollow(target).is_err() {
                    return error_response("unfollow_failed", &e.to_string());
                }
            } else {
                return error_response("unfollow_failed", &e.to_string());
            }
        }
    }

    // Notify relay (best-effort)
    notify_relay_unfollow(state, &resolved.node_id).await;

    ok_response(None)
}

pub async fn handle_block(state: &Arc<NodeState>, target: &str) -> Response {
    // Resolve @username → node_id
    let resolved = match resolve_target(state, target).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    {
        let mut follow_store = state.follow_store.lock().await;
        if let Err(e) = follow_store.block(&resolved.node_id) {
            return error_response("block_failed", &e.to_string());
        }
    }

    // Also unfollow on the relay
    notify_relay_unfollow(state, &resolved.node_id).await;

    ok_response(None)
}

pub async fn handle_following(state: &Arc<NodeState>) -> Response {
    let follow_store = state.follow_store.lock().await;
    let list: Vec<FollowInfo> = follow_store
        .following()
        .iter()
        .map(|f| FollowInfo {
            node_id: f.node_id.clone(),
            username: f.username.clone(),
            followed_at_ms: f.followed_at_ms,
        })
        .collect();
    ok_response(Some(serde_json::to_value(list).unwrap()))
}

pub async fn handle_followers(state: &Arc<NodeState>) -> Response {
    if state.relay_hosts.is_empty() {
        return error_response("no_relay", "not connected to any relay");
    }

    match fetch_followers_from_relay(state, &state.identity.node_id).await {
        Ok(entries) => {
            let list: Vec<FollowInfo> = entries
                .into_iter()
                .map(|e| FollowInfo {
                    node_id: e.node_id,
                    username: if e.username.is_empty() {
                        None
                    } else {
                        Some(e.username)
                    },
                    followed_at_ms: 0, // relay doesn't expose this currently
                })
                .collect();
            ok_response(Some(serde_json::to_value(list).unwrap()))
        }
        Err(e) => error_response("relay_unavailable", &e),
    }
}

pub async fn handle_register_username(state: &Arc<NodeState>, username: &str) -> Response {
    if state.relay_hosts.is_empty() {
        return error_response("no_relay", "not connected to any relay");
    }

    let sig = match state.identity.sign(state.identity.node_id.as_bytes()) {
        Ok(s) => s,
        Err(e) => return error_response("sign_error", &format!("failed to sign: {e}")),
    };

    for host in &state.relay_hosts {
        let mut client = match state.get_grpc_client(host).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "failed to connect for username registration");
                continue;
            }
        };

        match client
            .register_username(host_pb::RegisterUsernameRequest {
                username: username.to_string(),
                node_id: state.identity.node_id.clone(),
                public_key_b64: state.identity.public_key_b64.clone(),
                signature_b64: sig.clone(),
            })
            .await
        {
            Ok(resp) => {
                let r = resp.into_inner();
                if r.success {
                    *state.username.lock().await = Some(username.to_lowercase());
                    return ok_response(Some(
                        serde_json::json!({ "username": username.to_lowercase() }),
                    ));
                }
                let err = r.error.unwrap_or_default();
                return error_response("registration_failed", &err);
            }
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "register_username RPC failed");
                continue;
            }
        }
    }

    error_response(
        "relay_unavailable",
        "could not reach any relay for username registration",
    )
}

pub async fn handle_lookup_username(state: &Arc<NodeState>, username: &str) -> Response {
    if state.relay_hosts.is_empty() {
        return error_response("no_relay", "not connected to any relay");
    }

    for host in &state.relay_hosts {
        let mut client = match state.get_grpc_client(host).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "failed to connect for username lookup");
                continue;
            }
        };

        match client
            .lookup_username(host_pb::LookupUsernameRequest {
                username: username.to_string(),
            })
            .await
        {
            Ok(resp) => {
                let r = resp.into_inner();
                if r.found {
                    return ok_response(Some(serde_json::json!({
                        "username": username.to_lowercase(),
                        "node_id": r.node_id,
                        "public_key_b64": r.public_key_b64,
                    })));
                }
                return error_response(
                    "not_found",
                    &format!("username @{} not found", username.to_lowercase()),
                );
            }
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "lookup_username RPC failed");
                continue;
            }
        }
    }

    error_response(
        "relay_unavailable",
        "could not reach any relay for username lookup",
    )
}

pub async fn handle_sync_push(state: &Arc<NodeState>, confirm: bool) -> Response {
    if !confirm {
        return error_response(
            "confirm_required",
            "pass --confirm to push local follows to relay",
        );
    }

    if state.relay_hosts.is_empty() {
        return error_response("no_relay", "not connected to any relay");
    }

    let following = {
        let follow_store = state.follow_store.lock().await;
        follow_store.following().to_vec()
    };

    let mut pushed = 0usize;
    for record in &following {
        notify_relay_follow(state, &record.node_id).await;
        pushed += 1;
    }

    let result = SyncResult {
        pushed: Some(pushed),
        pulled: None,
        added: None,
        updated: None,
    };
    ok_response(Some(serde_json::to_value(result).unwrap()))
}

/// Fetch following list from relay via GetFollowing RPC.
pub(crate) async fn fetch_following_from_relay(
    state: &Arc<NodeState>,
    node_id: &str,
) -> Result<Vec<host_pb::FollowEntry>, String> {
    for host in &state.relay_hosts {
        let mut client = match state.get_grpc_client(host).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "failed to connect for GetFollowing");
                continue;
            }
        };

        match client
            .get_following(host_pb::GetFollowingRequest {
                node_id: node_id.to_string(),
            })
            .await
        {
            Ok(resp) => return Ok(resp.into_inner().following),
            Err(e) => {
                tracing::warn!(host = %host, err = %e, "GetFollowing RPC failed");
                continue;
            }
        }
    }

    Err("could not reach any relay for GetFollowing".to_string())
}

pub async fn handle_sync_pull(state: &Arc<NodeState>, confirm: bool) -> Response {
    if !confirm {
        return error_response(
            "confirm_required",
            "pass --confirm to pull follows from relay into local store",
        );
    }

    if state.relay_hosts.is_empty() {
        return error_response("no_relay", "not connected to any relay");
    }

    match sync_pull_from_relay(state).await {
        Ok(result) => ok_response(Some(serde_json::to_value(result).unwrap())),
        Err(e) => error_response("relay_unavailable", &e),
    }
}

/// Core sync-pull logic, reusable for both the handler and auto-recovery on startup.
pub async fn sync_pull_from_relay(state: &Arc<NodeState>) -> Result<SyncResult, String> {
    let entries = fetch_following_from_relay(state, &state.identity.node_id).await?;

    let mut added = 0usize;
    let mut updated = 0usize;
    let pulled = entries.len();

    let mut follow_store = state.follow_store.lock().await;
    for entry in &entries {
        let already_following = follow_store
            .following()
            .iter()
            .any(|f| f.node_id == entry.node_id);

        let record = FollowRecord {
            node_id: entry.node_id.clone(),
            public_key_b64: entry.public_key_b64.clone(),
            username: if entry.username.is_empty() {
                None
            } else {
                Some(entry.username.clone())
            },
            relay_hints: vec![],
            followed_at_ms: now_ms(),
        };

        if let Err(e) = follow_store.follow(record) {
            tracing::warn!(node_id = %entry.node_id, err = %e, "sync-pull: failed to upsert follow");
            continue;
        }

        if already_following {
            updated += 1;
        } else {
            added += 1;
        }
    }

    Ok(SyncResult {
        pushed: None,
        pulled: Some(pulled),
        added: Some(added),
        updated: Some(updated),
    })
}
