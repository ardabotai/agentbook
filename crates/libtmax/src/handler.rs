use anyhow::{Result, anyhow};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

use crate::SessionManager;

/// Communication policy controlling which sessions can message each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommsPolicy {
    /// No hierarchy restrictions.
    Open,
    /// Sessions must share the same root ancestor.
    SameSubtree,
    /// Only direct parent/child routes allowed.
    ParentOnly,
}

pub fn parse_comms_policy(raw: &str) -> Result<CommsPolicy> {
    match raw {
        "open" => Ok(CommsPolicy::Open),
        "same_subtree" => Ok(CommsPolicy::SameSubtree),
        "parent_only" => Ok(CommsPolicy::ParentOnly),
        _ => Err(anyhow!(
            "invalid --comms-policy '{raw}' (expected open|same_subtree|parent_only)"
        )),
    }
}

/// Enforce comms policy between two sessions.
pub async fn enforce_comms_policy_for_pair(
    policy: CommsPolicy,
    manager: &SessionManager,
    from_session_id: &str,
    to_session_id: &str,
) -> Result<()> {
    if from_session_id == to_session_id || policy == CommsPolicy::Open {
        return Ok(());
    }

    match policy {
        CommsPolicy::Open => Ok(()),
        CommsPolicy::SameSubtree => {
            let from_root = session_root(manager, from_session_id).await?;
            let to_root = session_root(manager, to_session_id).await?;
            if from_root == to_root {
                Ok(())
            } else {
                Err(anyhow!(
                    "comms policy same_subtree denied route {} -> {}",
                    from_session_id,
                    to_session_id
                ))
            }
        }
        CommsPolicy::ParentOnly => {
            let from_parent = session_parent(manager, from_session_id).await?;
            let to_parent = session_parent(manager, to_session_id).await?;
            let allowed = from_parent.as_deref() == Some(to_session_id)
                || to_parent.as_deref() == Some(from_session_id);
            if allowed {
                Ok(())
            } else {
                Err(anyhow!(
                    "comms policy parent_only denied route {} -> {}",
                    from_session_id,
                    to_session_id
                ))
            }
        }
    }
}

/// Resolve the sender session from an explicit value or the connection's attachments.
pub fn resolve_sender_session(
    explicit: Option<String>,
    owned_attachments: &HashMap<String, String>,
) -> Result<Option<String>> {
    if owned_attachments.is_empty() {
        return Ok(explicit);
    }

    if let Some(session_id) = explicit {
        if bound_sessions(owned_attachments).contains(&session_id) {
            return Ok(Some(session_id));
        }
        return Err(anyhow!("session access denied for this connection"));
    }

    let mut bound = bound_sessions(owned_attachments).into_iter();
    let first = bound.next();
    if bound.next().is_some() {
        return Err(anyhow!(
            "multiple attached sessions; specify an explicit sender session"
        ));
    }
    Ok(first)
}

/// Enforce that a session is bound on this connection (if any attachments exist).
pub fn enforce_session_binding_if_present(
    session_id: &str,
    owned_attachments: &HashMap<String, String>,
) -> Result<()> {
    if owned_attachments.is_empty() {
        return Ok(());
    }
    if bound_sessions(owned_attachments).contains(session_id) {
        return Ok(());
    }
    Err(anyhow!("session access denied for this connection"))
}

pub fn bound_sessions(owned_attachments: &HashMap<String, String>) -> HashSet<String> {
    owned_attachments.values().cloned().collect()
}

pub async fn session_parent(manager: &SessionManager, session_id: &str) -> Result<Option<String>> {
    let info = manager
        .session_info(session_id)
        .await
        .ok_or_else(|| anyhow!("session not found"))?;
    Ok(info.parent_id)
}

pub async fn session_root(manager: &SessionManager, session_id: &str) -> Result<String> {
    let mut current = session_id.to_string();
    let mut guard = 0usize;
    loop {
        guard = guard.saturating_add(1);
        if guard > 256 {
            return Err(anyhow!("session tree depth exceeded while resolving root"));
        }
        let parent = session_parent(manager, &current).await?;
        if let Some(parent) = parent {
            current = parent;
        } else {
            return Ok(current);
        }
    }
}

pub async fn task_created_by_session(
    manager: &SessionManager,
    task_id: &str,
) -> Result<Option<String>> {
    let task = manager
        .task_by_id(task_id)
        .await
        .ok_or_else(|| anyhow!("task not found"))?;
    Ok(task.created_by)
}

pub async fn task_policy_peer_session(
    manager: &SessionManager,
    task_id: &str,
    actor_session_id: &str,
) -> Result<Option<String>> {
    let task = manager
        .task_by_id(task_id)
        .await
        .ok_or_else(|| anyhow!("task not found"))?;

    if let Some(created_by) = task.created_by
        && created_by != actor_session_id
    {
        return Ok(Some(created_by));
    }
    if let Some(assignee) = task.assignee_session_id
        && assignee != actor_session_id
    {
        return Ok(Some(assignee));
    }
    Ok(None)
}

/// Serialize a value to a serde_json::Value, wrapping in Response::Ok.
pub fn ok_response<T: Serialize>(data: &T) -> Result<tmax_protocol::Response> {
    Ok(tmax_protocol::Response::ok(Some(serde_json::to_value(
        data,
    )?)))
}

/// Try-send a response on an mpsc channel. Returns error if full or closed.
pub fn enqueue_response(
    out_tx: &tokio::sync::mpsc::Sender<tmax_protocol::Response>,
    response: tmax_protocol::Response,
) -> Result<()> {
    match out_tx.try_send(response) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            Err(anyhow!("client outbound queue full"))
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(anyhow!("connection closed")),
    }
}
