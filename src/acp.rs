use crate::router::{SessionRouter, StdoutTx};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

fn success_response(id: Value, result: Value) -> String {
    serde_json::to_string(&RpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    })
    .unwrap()
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    serde_json::to_string(&RpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(RpcError {
            code,
            message: message.into(),
        }),
    })
    .unwrap()
}

pub async fn handle_request(
    req: RpcRequest,
    router: &Arc<SessionRouter>,
    stdout_tx: &StdoutTx,
) {
    let id = req.id.clone().unwrap_or(Value::Null);

    let response = match req.method.as_str() {
        "initialize" => handle_initialize(id.clone()),
        "session/new" => handle_session_new(id.clone(), &req.params, router),
        "session/load" => handle_session_load(id.clone()),
        "session/prompt" => handle_session_prompt(id.clone(), &req.params, router).await,
        "session/list" => handle_session_list(id.clone(), router),
        "session/set_mode" => success_response(id.clone(), json!({})),
        _ => error_response(id.clone(), -32601, &format!("method not found: {}", req.method)),
    };

    stdout_tx.send(response).ok();
}

fn handle_initialize(id: Value) -> String {
    success_response(
        id,
        json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": false,
                "sessionCapabilities": {
                    "list": {}
                }
            }
        }),
    )
}

fn handle_session_new(id: Value, params: &Value, router: &Arc<SessionRouter>) -> String {
    let cwd = params
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    let session_id = format!("pty-{}", uuid::Uuid::new_v4());

    match router.create_session(&session_id, cwd) {
        Ok(()) => {
            tracing::info!(session_id = %session_id, cwd = %cwd, "session created");
            success_response(id, json!({ "sessionId": session_id }))
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create session");
            error_response(id, -32600, &format!("failed to create session: {}", e))
        }
    }
}

fn handle_session_load(id: Value) -> String {
    error_response(id, -32600, "PTY sessions cannot be restored")
}

async fn handle_session_prompt(id: Value, params: &Value, router: &Arc<SessionRouter>) -> String {
    let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
        Some(sid) => sid,
        None => return error_response(id, -32602, "missing sessionId"),
    };

    if !router.has_session(session_id) {
        return error_response(
            id,
            -32600,
            &format!("session not found: {}", session_id),
        );
    }

    let text = params
        .get("prompt")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match router.handle_prompt(session_id, text).await {
        Ok(()) => success_response(id, json!({})),
        Err(e) => error_response(id, -32600, &e.to_string()),
    }
}

fn handle_session_list(id: Value, router: &Arc<SessionRouter>) -> String {
    let sessions = router.list_sessions();
    success_response(id, json!({ "sessions": sessions }))
}
