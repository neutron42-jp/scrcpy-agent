//! Minimal, self-contained MCP server over Streamable HTTP.
//!
//! Tools only: initialize, ping, tools/list and tools/call. Requests are
//! stateless (2026-07-28 style); legacy clients that open a session still work
//! because no session id is required.

use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::app::AppState;
use crate::tools;

pub struct McpHandle {
    pub url: String,
    pub shutdown: Option<oneshot::Sender<()>>,
    pub join: Option<tokio::task::JoinHandle<()>>,
}

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("create tokio runtime")
    })
}

const SUPPORTED_PROTOCOLS: &[&str] = &["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"];

const INSTRUCTIONS: &str = "Controls the Android device selected in the scrcpy-agent app. \
Screenshots return the current screen; every coordinate argument is a pixel in \
that screenshot's coordinate space (width and height accompany each image). \
Take a screenshot after each action to confirm the result. type_text pastes \
through the device clipboard by default, which is safe with any IME; pass \
mode=key to inject raw key events instead.";

pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let app = Router::new()
        .route("/mcp", post(handle_post).get(handle_get))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await?;
    Ok(())
}

async fn handle_get() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "this MCP server has no server-initiated stream; use POST /mcp",
    )
        .into_response()
}

async fn handle_post(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let cfg = state.cfg.lock().unwrap().clone();
    if !origin_allowed(&headers, cfg.mcp_port) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    if let Some(token) = cfg.auth_token.as_deref() {
        let expected = format!("Bearer {token}");
        let provided = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if provided != expected {
            return (StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response();
        }
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return rpc_response(
                StatusCode::OK,
                wants_event_stream(&headers),
                &[json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": format!("parse error: {err}")}
                })],
            );
        }
    };

    let responses = match payload {
        Value::Array(messages) => {
            let mut responses = Vec::new();
            for message in messages {
                if let Some(response) = dispatch_message(&state, message).await {
                    responses.push(response);
                }
            }
            responses
        }
        message => dispatch_message(&state, message)
            .await
            .into_iter()
            .collect(),
    };

    if responses.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    rpc_response(StatusCode::OK, wants_event_stream(&headers), &responses)
}

async fn dispatch_message(state: &Arc<AppState>, message: Value) -> Option<Value> {
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let id = message.get("id").cloned();
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    if id.is_none() {
        if method == "notifications/initialized" {
            log::debug!("MCP client initialized");
        }
        return None;
    }
    let id = id.unwrap();

    let result = match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(SUPPORTED_PROTOCOLS[0]);
            let negotiated = if SUPPORTED_PROTOCOLS.contains(&requested) {
                requested
            } else {
                SUPPORTED_PROTOCOLS[0]
            };
            Ok(json!({
                "protocolVersion": negotiated,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "android", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools::definitions()})),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if name.is_empty() {
                Err((-32602, "tools/call requires a tool name".to_string()))
            } else {
                let output = tools::call(Arc::clone(state), &name, arguments).await;
                state.notify_tool_call(&name, !output.is_error, &output.summary());
                let mut result = json!({
                    "content": output.content,
                    "isError": output.is_error,
                });
                if let Some(structured) = output.structured {
                    result["structuredContent"] = structured;
                }
                Ok(result)
            }
        }
        other => Err((-32601, format!("method not found: {other}"))),
    };

    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    })
}

fn wants_event_stream(headers: &HeaderMap) -> bool {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    accept.contains("text/event-stream") && !accept.contains("application/json")
}

fn rpc_response(status: StatusCode, sse: bool, messages: &[Value]) -> Response {
    if sse {
        let mut body = String::new();
        for message in messages {
            body.push_str("event: message\ndata: ");
            body.push_str(&message.to_string());
            body.push_str("\n\n");
        }
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(axum::body::Body::from(body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let body = if messages.len() == 1 {
        messages[0].clone()
    } else {
        Value::Array(messages.to_vec())
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn origin_allowed(headers: &HeaderMap, port: u16) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let allowed = [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
    ];
    allowed.iter().any(|candidate| candidate == origin)
}
