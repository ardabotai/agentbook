use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::Value;

use serde::Deserialize;
use tmax_protocol::{Request, Response as TmaxResponse};

use crate::ws::AppState;

/// Request body for POST /api/sessions.
#[derive(Deserialize)]
pub struct CreateSessionRequest {
    #[serde(default = "default_exec")]
    pub exec: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_exec() -> String {
    "/bin/bash".to_string()
}
fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

/// POST /api/sessions - Create a new session.
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    let mut client = crate::client::connect(Some(&state.socket_path))
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("server unavailable: {e}")))?;

    let req = Request::SessionCreate {
        exec: body.exec,
        args: body.args,
        cwd: None,
        label: body.label,
        sandbox: None,
        parent_id: None,
        cols: body.cols,
        rows: body.rows,
    };

    let resp = client
        .request(&req)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("request failed: {e}")))?;

    match resp {
        TmaxResponse::Ok { data } => Ok((
            StatusCode::CREATED,
            Json(data.unwrap_or(Value::Null)),
        )),
        TmaxResponse::Error { message, .. } => {
            Err((StatusCode::INTERNAL_SERVER_ERROR, message))
        }
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected response".to_string(),
        )),
    }
}

/// GET /api/sessions - List all sessions.
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut client = crate::client::connect(Some(&state.socket_path))
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("server unavailable: {e}")))?;

    let resp = client
        .request(&Request::SessionList)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("request failed: {e}")))?;

    match resp {
        TmaxResponse::Ok { data } => Ok(Json(data.unwrap_or(Value::Array(vec![])))),
        TmaxResponse::Error { message, .. } => {
            Err((StatusCode::INTERNAL_SERVER_ERROR, message))
        }
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected response".to_string(),
        )),
    }
}

/// GET /api/sessions/tree - Session hierarchy.
pub async fn session_tree(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut client = crate::client::connect(Some(&state.socket_path))
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("server unavailable: {e}")))?;

    let resp = client
        .request(&Request::SessionTree)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("request failed: {e}")))?;

    match resp {
        TmaxResponse::Ok { data } => Ok(Json(data.unwrap_or(Value::Array(vec![])))),
        TmaxResponse::Error { message, .. } => {
            Err((StatusCode::INTERNAL_SERVER_ERROR, message))
        }
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected response".to_string(),
        )),
    }
}

/// GET /api/sessions/:id - Session details.
pub async fn session_info(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut client = crate::client::connect(Some(&state.socket_path))
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("server unavailable: {e}")))?;

    let resp = client
        .request(&Request::SessionInfo { session_id })
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("request failed: {e}")))?;

    match resp {
        TmaxResponse::Ok { data } => Ok(Json(data.unwrap_or(Value::Null))),
        TmaxResponse::Error { message, code } => {
            let status = match code {
                tmax_protocol::ErrorCode::SessionNotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err((status, message))
        }
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected response".to_string(),
        )),
    }
}
