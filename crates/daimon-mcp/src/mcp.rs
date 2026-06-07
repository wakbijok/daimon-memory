//! Minimal **MCP-over-HTTP (JSON-RPC 2.0)** surface for daimon-memory.
//!
//! Implements the request/response subset: `initialize`, `tools/list`, `tools/call`
//! (+ `ping`), dispatching the three tools - `remember`, `recall`, `read` - to the
//! deterministic engine. Full streamable-HTTP/SSE (server→client streaming) is a
//! later slice; this synchronous JSON-RPC is what most clients exercise for tool use.

use crate::AppState;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use daimon_memory_core::{
    ContextMemory, ContextScope, MemoryKind, MemoryUri, MemoryWrite, RecallFilters,
};
use serde_json::{Value, json};
use uuid::Uuid;

const PROTOCOL_VERSION: &str = "2024-11-05";

fn ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn err(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
fn tool_text(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}
fn tool_err(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn tenant_from(headers: &HeaderMap, default: Uuid) -> Uuid {
    headers
        .get("x-daimon-tenant")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or(default)
}

fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "remember",
                "description": "Store a typed memory. The control layer validates the schema; recall is deterministic.",
                "inputSchema": {
                    "type": "object",
                    "required": ["kind", "namespace", "title", "body"],
                    "properties": {
                        "kind": {"type": "string", "description": "decision|runbook|incident_summary|service_topology|known_failure_mode|remediation_pattern|project_convention|agent_lesson|resource_summary"},
                        "namespace": {"type": "string", "description": "e.g. shared-canonical/coding/decisions"},
                        "title": {"type": "string"},
                        "body": {"type": "string"},
                        "fields": {"type": "object", "description": "kind-specific required fields (e.g. decision needs context+rationale)"},
                        "tags": {"type": "array", "items": {"type": "string"}},
                        "importance": {"type": "integer", "minimum": 0, "maximum": 100}
                    }
                }
            },
            {
                "name": "recall",
                "description": "Deterministic recall (full-text + filters, no LLM). Returns ranked hits with abstract + uri.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "kind": {"type": "string"},
                        "namespace_prefix": {"type": "string"},
                        "limit": {"type": "integer"}
                    }
                }
            },
            {
                "name": "read",
                "description": "Fetch a memory's full content by its daimon:// uri.",
                "inputSchema": {
                    "type": "object",
                    "required": ["uri"],
                    "properties": {"uri": {"type": "string"}}
                }
            }
        ]
    })
}

pub async fn handle(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    // A JSON-RPC notification has no `id` (e.g. notifications/initialized). The streamable-HTTP
    // transport requires 202 Accepted with an EMPTY body for these - NOT a JSON response.
    // Lenient clients tolerate a `null` body; strict ones (Codex's rmcp) close the channel.
    if req.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }

    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    if method == "initialize" {
        // Assign a session id (streamable-HTTP). We are stateless, so we never require it back.
        let session = Uuid::new_v4().to_string();
        let body = ok(
            &id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "daimon-memory", "version": env!("CARGO_PKG_VERSION")}
            }),
        );
        return ([("mcp-session-id", session)], Json(body)).into_response();
    }

    let resp = match method {
        "ping" => ok(&id, json!({})),
        "tools/list" => ok(&id, tool_definitions()),
        "tools/call" => call_tool(&st, &headers, &id, &params).await,
        _ => err(&id, -32601, "method not found"),
    };
    Json(resp).into_response()
}

async fn call_tool(st: &AppState, headers: &HeaderMap, id: &Value, params: &Value) -> Value {
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let scope = ContextScope::tenant(tenant_from(headers, st.default_tenant));

    match name {
        "remember" => {
            let kind = match args
                .get("kind")
                .and_then(|k| k.as_str())
                .and_then(|s| MemoryKind::parse(s).ok())
            {
                Some(k) => k,
                None => return ok(id, tool_err("invalid or missing 'kind'".into())),
            };
            let getstr = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let fields = args
                .get("fields")
                .and_then(|f| f.as_object())
                .cloned()
                .unwrap_or_default();
            let tags = args
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let importance = args.get("importance").and_then(|i| i.as_u64()).unwrap_or(0) as u8;
            let w = MemoryWrite {
                kind,
                namespace: getstr("namespace"),
                title: getstr("title"),
                body: getstr("body"),
                fields,
                source_refs: vec![],
                tags,
                importance,
                confidence: 1.0,
            };
            match st.store.store(&scope, w).await {
                Ok(uri) => ok(id, tool_text(format!("stored: {uri}"))),
                Err(e) => ok(id, tool_err(format!("{e}"))),
            }
        }
        "recall" => {
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("").to_string();
            let filters = RecallFilters {
                kind: args
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .and_then(|s| MemoryKind::parse(s).ok()),
                namespace_prefix: args
                    .get("namespace_prefix")
                    .and_then(|n| n.as_str())
                    .map(String::from),
                project_id: None,
                since: None,
                limit: args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize,
            };
            match st.store.find(&scope, &query, &filters).await {
                Ok(hits) => ok(id, tool_text(serde_json::to_string_pretty(&hits).unwrap_or_default())),
                Err(e) => ok(id, tool_err(format!("{e}"))),
            }
        }
        "read" => {
            let uri = args.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            match MemoryUri::parse(uri) {
                Ok(u) => match st.store.read(&scope, &u).await {
                    Ok(rec) => ok(id, tool_text(serde_json::to_string_pretty(&rec).unwrap_or_default())),
                    Err(e) => ok(id, tool_err(format!("{e}"))),
                },
                Err(e) => ok(id, tool_err(format!("{e}"))),
            }
        }
        other => err(id, -32602, &format!("unknown tool: {other}")),
    }
}
