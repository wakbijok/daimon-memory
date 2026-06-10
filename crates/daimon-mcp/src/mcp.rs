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
    strip_tenant_segment,
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
                        "kind": {"type": "string", "description": "decision|runbook|incident_summary|service_topology|known_failure_mode|remediation_pattern|project_convention|agent_lesson|resource_summary|persona|protocol|reminder"},
                        "namespace": {"type": "string", "description": "e.g. resources/coding/decisions"},
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
                "description": "Deterministic hybrid recall (full-text + semantic vectors, RRF-fused, no LLM). Returns ranked hits with scores, abstract + uri.",
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
            },
            {
                "name": "log_decision",
                "description": "Persist a non-obvious DECISION (chose A over B). State what was rejected. Call the moment a real choice is made.",
                "inputSchema": {"type": "object", "required": ["title", "context", "rationale"], "properties": {
                    "title": {"type": "string"},
                    "context": {"type": "string", "description": "the situation that prompted the choice"},
                    "rationale": {"type": "string", "description": "why this option; trade-offs; what was rejected"},
                    "body": {"type": "string", "description": "optional full detail; defaults to context + rationale"},
                    "namespace": {"type": "string", "description": "default resources/coding/decisions"},
                    "importance": {"type": "integer", "minimum": 0, "maximum": 100}
                }}
            },
            {
                "name": "log_lesson",
                "description": "Persist a reusable LESSON or a corrected mistake. Call immediately when corrected or when you learn something not to repeat.",
                "inputSchema": {"type": "object", "required": ["title", "lesson"], "properties": {
                    "title": {"type": "string"},
                    "lesson": {"type": "string", "description": "the reusable insight, phrased so it prevents the mistake next time"},
                    "body": {"type": "string"},
                    "namespace": {"type": "string", "description": "default agent/lessons"},
                    "importance": {"type": "integer", "minimum": 0, "maximum": 100}
                }}
            },
            {
                "name": "log_incident",
                "description": "Persist an INCIDENT / failure (regression, rollback, outage, data loss, wasted effort). No blame. Ask the user before logging if unsure.",
                "inputSchema": {"type": "object", "required": ["title", "impact", "resolution"], "properties": {
                    "title": {"type": "string"},
                    "impact": {"type": "string", "description": "what was lost or affected"},
                    "resolution": {"type": "string", "description": "how it was resolved"},
                    "severity": {"type": "string", "description": "optional: minor|moderate|major"},
                    "prevention": {"type": "string", "description": "optional: specific action to prevent recurrence"},
                    "body": {"type": "string"},
                    "namespace": {"type": "string", "description": "default resources/incidents"},
                    "importance": {"type": "integer", "minimum": 0, "maximum": 100}
                }}
            },
            {
                "name": "add_reminder",
                "description": "Persist a dated FOLLOW-UP / next-session item.",
                "inputSchema": {"type": "object", "required": ["title", "due"], "properties": {
                    "title": {"type": "string"},
                    "due": {"type": "string", "description": "absolute date or datetime, e.g. 2026-06-15"},
                    "body": {"type": "string"},
                    "namespace": {"type": "string", "description": "default agent/workstream"},
                    "importance": {"type": "integer", "minimum": 0, "maximum": 100}
                }}
            },
            {
                "name": "browse",
                "description": "List memory uris under a namespace prefix. Use before saving to avoid duplicates, or to explore what exists.",
                "inputSchema": {"type": "object", "required": ["prefix"], "properties": {
                    "prefix": {"type": "string", "description": "e.g. resources/coding/decisions"}
                }}
            },
            {
                "name": "forget",
                "description": "Retract a memory by uri (marks it forgotten). user/ and session/ records forget freely; durable agent/ and resources/ records require confirm=true.",
                "inputSchema": {"type": "object", "required": ["uri"], "properties": {
                    "uri": {"type": "string"},
                    "confirm": {"type": "boolean", "description": "must be true to forget a durable agent/ or resources/ record"}
                }}
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
            let getstr = |k: &str| {
                args.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let fields = args
                .get("fields")
                .and_then(|f| f.as_object())
                .cloned()
                .unwrap_or_default();
            let tags = args
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
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
                Ok(uri) => ok(id, tool_text(format!("stored: {}", uri.display_relative()))),
                Err(e) => ok(id, tool_err(format!("{e}"))),
            }
        }
        "recall" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .unwrap_or("")
                .to_string();
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
            // Hybrid (keyword + semantic RRF) - the SAME path as /v1/recall, so the explicit
            // recall tool consults embeddings, not Postgres FTS alone. hybrid_recall already
            // emits tenant-relative URIs; stripping again here ate the namespace root and
            // broke the recall -> read loop.
            let hits = crate::hybrid_recall(st, &scope, &query, &filters).await;
            ok(
                id,
                tool_text(serde_json::to_string_pretty(&hits).unwrap_or_default()),
            )
        }
        "read" => {
            let uri = args.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            match MemoryUri::parse_scoped(uri, scope.tenant_id) {
                Ok(u) => match st.store.read(&scope, &u).await {
                    Ok(mut rec) => {
                        rec.uri = strip_tenant_segment(&rec.uri);
                        ok(
                            id,
                            tool_text(serde_json::to_string_pretty(&rec).unwrap_or_default()),
                        )
                    }
                    Err(e) => ok(id, tool_err(format!("{e}"))),
                },
                Err(e) => ok(id, tool_err(format!("{e}"))),
            }
        }
        "log_decision" => {
            let mut f = serde_json::Map::new();
            f.insert("context".into(), Value::String(sarg(&args, "context")));
            f.insert("rationale".into(), Value::String(sarg(&args, "rationale")));
            let body = body_or(
                &args,
                &format!("{}\n\n{}", sarg(&args, "context"), sarg(&args, "rationale")),
            );
            do_store(
                st,
                &scope,
                id,
                MemoryKind::Decision,
                ns_or(&args, "resources/coding/decisions"),
                sarg(&args, "title"),
                body,
                f,
                imp(&args),
            )
            .await
        }
        "log_lesson" => {
            let mut f = serde_json::Map::new();
            f.insert("lesson".into(), Value::String(sarg(&args, "lesson")));
            let body = body_or(&args, &sarg(&args, "lesson"));
            do_store(
                st,
                &scope,
                id,
                MemoryKind::AgentLesson,
                ns_or(&args, "agent/lessons"),
                sarg(&args, "title"),
                body,
                f,
                imp(&args),
            )
            .await
        }
        "log_incident" => {
            let mut f = serde_json::Map::new();
            f.insert("impact".into(), Value::String(sarg(&args, "impact")));
            f.insert(
                "resolution".into(),
                Value::String(sarg(&args, "resolution")),
            );
            let sev = sarg(&args, "severity");
            if !sev.is_empty() {
                f.insert("severity".into(), Value::String(sev));
            }
            let prev = sarg(&args, "prevention");
            if !prev.is_empty() {
                f.insert("prevention".into(), Value::String(prev));
            }
            let body = body_or(
                &args,
                &format!("{}\n\n{}", sarg(&args, "impact"), sarg(&args, "resolution")),
            );
            do_store(
                st,
                &scope,
                id,
                MemoryKind::IncidentSummary,
                ns_or(&args, "resources/incidents"),
                sarg(&args, "title"),
                body,
                f,
                imp(&args),
            )
            .await
        }
        "add_reminder" => {
            let mut f = serde_json::Map::new();
            f.insert("due".into(), Value::String(sarg(&args, "due")));
            let body = body_or(&args, &sarg(&args, "title"));
            do_store(
                st,
                &scope,
                id,
                MemoryKind::Reminder,
                ns_or(&args, "agent/workstream"),
                sarg(&args, "title"),
                body,
                f,
                imp(&args),
            )
            .await
        }
        "browse" => {
            let prefix = sarg(&args, "prefix");
            match st.store.list(&scope, &prefix).await {
                Ok(uris) => ok(
                    id,
                    tool_text(if uris.is_empty() {
                        "(no records under this prefix)".to_string()
                    } else {
                        uris.iter()
                            .map(|u| u.display_relative())
                            .collect::<Vec<_>>()
                            .join("\n")
                    }),
                ),
                Err(e) => ok(id, tool_err(format!("{e}"))),
            }
        }
        "forget" => {
            let uri_s = sarg(&args, "uri");
            let confirm = args
                .get("confirm")
                .and_then(|c| c.as_bool())
                .unwrap_or(false);
            // Durable buckets (agent/, resources/) are confirm-gated; user/ + session/ forget freely.
            let durable = uri_s.contains("/agent/") || uri_s.contains("/resources/");
            if durable && !confirm {
                return ok(
                    id,
                    tool_err("refusing to forget a durable agent/ or resources/ record without confirm=true".to_string()),
                );
            }
            match MemoryUri::parse_scoped(&uri_s, scope.tenant_id) {
                Ok(u) => match st.store.forget(&scope, &u).await {
                    Ok(()) => ok(id, tool_text(format!("forgotten: {uri_s}"))),
                    Err(e) => ok(id, tool_err(format!("{e}"))),
                },
                Err(e) => ok(id, tool_err(format!("{e}"))),
            }
        }
        other => err(id, -32602, &format!("unknown tool: {other}")),
    }
}

fn sarg(args: &Value, k: &str) -> String {
    args.get(k)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}
fn imp(args: &Value) -> u8 {
    args.get("importance").and_then(|i| i.as_u64()).unwrap_or(0) as u8
}
fn ns_or(args: &Value, default: &str) -> String {
    let n = args.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    if n.is_empty() {
        default.to_string()
    } else {
        n.to_string()
    }
}
fn body_or(args: &Value, default: &str) -> String {
    let b = sarg(args, "body");
    if b.is_empty() { default.to_string() } else { b }
}

/// Build a [`MemoryWrite`] from a fixed kind + named fields and store it.
/// Shared by the guided `log_*` / `add_reminder` tools.
#[allow(clippy::too_many_arguments)]
async fn do_store(
    st: &AppState,
    scope: &ContextScope,
    id: &Value,
    kind: MemoryKind,
    namespace: String,
    title: String,
    body: String,
    fields: serde_json::Map<String, Value>,
    importance: u8,
) -> Value {
    let w = MemoryWrite {
        kind,
        namespace,
        title,
        body,
        fields,
        source_refs: vec![],
        tags: vec![],
        importance,
        confidence: 1.0,
    };
    match st.store.store(scope, w).await {
        Ok(uri) => ok(id, tool_text(format!("stored: {}", uri.display_relative()))),
        Err(e) => ok(id, tool_err(format!("{e}"))),
    }
}
