use super::{NodeState, error_response, now_ms, ok_response};
use agentbook::protocol::{FollowInfo, HealthStatus, IdentityInfo, Response};
use agentbook_proto::host::v1 as host_pb;
use std::sync::Arc;

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
    let mut follow_store = state.follow_store.lock().await;

    let record = agentbook_mesh::follow::FollowRecord {
        node_id: target.to_string(),
        public_key_b64: String::new(),
        username: None,
        relay_hints: vec![],
        followed_at_ms: now_ms(),
    };

    match follow_store.follow(record) {
        Ok(()) => ok_response(None),
        Err(e) => error_response("follow_failed", &e.to_string()),
    }
}

pub async fn handle_unfollow(state: &Arc<NodeState>, target: &str) -> Response {
    let mut follow_store = state.follow_store.lock().await;
    match follow_store.unfollow(target) {
        Ok(()) => ok_response(None),
        Err(e) => error_response("unfollow_failed", &e.to_string()),
    }
}

pub async fn handle_block(state: &Arc<NodeState>, target: &str) -> Response {
    let mut follow_store = state.follow_store.lock().await;
    match follow_store.block(target) {
        Ok(()) => ok_response(None),
        Err(e) => error_response("block_failed", &e.to_string()),
    }
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

pub async fn handle_followers(_state: &Arc<NodeState>) -> Response {
    // For now, we don't track who follows us -- that requires the other side to announce.
    let list: Vec<FollowInfo> = vec![];
    ok_response(Some(serde_json::to_value(list).unwrap()))
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
