"""daimon-memory provider for Hermes Agent.

A Hermes external **memory provider** that backs recall + capture with daimon-memory
(the shared, cross-tool context plane). Mirrors the bundled `openviking` provider's
shape, but follows daimon-memory's two principles:

  1. Recall is deterministic / zero-LLM   -> prefetch() calls /v1/recall (hybrid).
  2. Capture is curated, never raw         -> sync_turn() is a deliberate NO-OP.
       * on_memory_write() mirrors Hermes's OWN curated memory writes into daimon.
       * the daimon_remember tool lets the agent persist typed records explicitly,
         validated by daimon-memory's control layer (it rejects malformed writes).

No raw-turn dumping, no extraction VLM on our side. Hermes does its small-scope
extraction; we persist the result as a typed, deterministic record.

Config (env, set by the installer into ~/.hermes/.env):
  DAIMON_ENDPOINT   (required)  e.g. http://10.100.30.27  or  http://127.0.0.1:18080
  DAIMON_TENANT     (optional)  tenant UUID; default the dev tenant
  DAIMON_NAMESPACE  (optional)  default capture namespace; default hermes-private/notes
  DAIMON_API_KEY    (optional)  bearer token, if daimon-mcp auth is enabled
"""

from __future__ import annotations

import json
import logging
import os
import threading
from typing import Any, Dict, List, Optional

from agent.memory_provider import MemoryProvider

logger = logging.getLogger(__name__)

_DEFAULT_TENANT = "00000000-0000-0000-0000-0000000000d1"
_DEFAULT_NAMESPACE = "hermes-private/notes"
_RECALL_LIMIT = 6
_HTTP_TIMEOUT = 6.0


def _get_httpx():
    """Lazy import so a missing dep degrades gracefully instead of breaking load."""
    try:
        import httpx  # type: ignore

        return httpx
    except Exception:  # pragma: no cover
        return None


# ---------------------------------------------------------------------------
# Tool schemas (OpenAI function-calling format)
# ---------------------------------------------------------------------------

_KINDS = (
    "decision, runbook, incident_summary, service_topology, known_failure_mode, "
    "remediation_pattern, project_convention, agent_lesson, resource_summary"
)

DAIMON_REMEMBER_SCHEMA = {
    "name": "daimon_remember",
    "description": (
        "Persist a durable, TYPED memory to daimon-memory (shared across tools). "
        "Use for decisions, runbooks, lessons, conventions — not chit-chat. The "
        "control layer validates required fields per kind and rejects malformed writes.\n"
        f"kinds: {_KINDS}.\n"
        "Required fields by kind: decision={context,rationale}; runbook={steps}; "
        "incident_summary={impact,resolution}; service_topology={service,dependencies}; "
        "known_failure_mode={symptom,cause}; remediation_pattern={problem,fix}; "
        "project_convention={rule}; agent_lesson={lesson}; resource_summary={source}."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "kind": {"type": "string", "description": f"One of: {_KINDS}"},
            "title": {"type": "string", "description": "Short title."},
            "body": {"type": "string", "description": "Full content."},
            "fields": {
                "type": "object",
                "description": "Kind-specific required fields (see description).",
            },
            "namespace": {
                "type": "string",
                "description": "Optional. Default per config (e.g. hermes-private/notes "
                "or shared-canonical/<area> for team-wide knowledge).",
            },
            "tags": {"type": "array", "items": {"type": "string"}},
            "importance": {"type": "integer", "description": "0-100 rerank boost."},
        },
        "required": ["kind", "title", "body"],
    },
}

DAIMON_RECALL_SCHEMA = {
    "name": "daimon_recall",
    "description": (
        "Search daimon-memory (hybrid keyword + semantic, no LLM). Returns ranked hits "
        "with daimon:// uris. Recall also runs automatically each turn — use this for "
        "explicit, targeted lookups."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "query": {"type": "string"},
            "kind": {"type": "string", "description": f"Optional filter. One of: {_KINDS}"},
            "limit": {"type": "integer", "description": "Max results (default 10)."},
        },
        "required": ["query"],
    },
}

DAIMON_READ_SCHEMA = {
    "name": "daimon_read",
    "description": "Fetch a memory's full content by its daimon:// uri.",
    "parameters": {
        "type": "object",
        "properties": {"uri": {"type": "string"}},
        "required": ["uri"],
    },
}


# ---------------------------------------------------------------------------
# HTTP client
# ---------------------------------------------------------------------------

class _DaimonClient:
    """Thin synchronous HTTP client for the daimon-memory REST API (/v1)."""

    def __init__(self, endpoint: str, tenant: str, api_key: str = ""):
        self._endpoint = endpoint.rstrip("/")
        self._tenant = tenant
        self._httpx = _get_httpx()
        headers = {"x-daimon-tenant": tenant, "content-type": "application/json"}
        if api_key:
            headers["authorization"] = f"Bearer {api_key}"
        self._client = (
            self._httpx.Client(timeout=_HTTP_TIMEOUT, headers=headers)
            if self._httpx
            else None
        )

    def healthy(self) -> bool:
        if not self._client:
            return False
        try:
            r = self._client.get(f"{self._endpoint}/readyz")
            return r.status_code == 200
        except Exception:
            return False

    def recall(self, query: str, kind: Optional[str] = None, limit: int = _RECALL_LIMIT) -> List[dict]:
        if not self._client or not query.strip():
            return []
        filters: Dict[str, Any] = {"limit": limit}
        if kind:
            filters["kind"] = kind
        try:
            r = self._client.post(
                f"{self._endpoint}/v1/recall", json={"query": query, "filters": filters}
            )
            r.raise_for_status()
            return r.json().get("hits", []) or []
        except Exception as e:
            logger.debug("daimon recall failed: %s", e)
            return []

    def store(self, payload: dict) -> dict:
        if not self._client:
            return {"error": "httpx unavailable"}
        try:
            r = self._client.post(f"{self._endpoint}/v1/memory", json=payload)
            if r.status_code == 400:
                return {"error": "validation", "detail": r.json().get("detail", "")}
            r.raise_for_status()
            return r.json()
        except Exception as e:
            return {"error": "backend", "detail": str(e)}

    def read(self, uri: str) -> dict:
        if not self._client:
            return {"error": "httpx unavailable"}
        try:
            r = self._client.get(f"{self._endpoint}/v1/read", params={"uri": uri})
            if r.status_code == 404:
                return {"error": "not_found", "uri": uri}
            r.raise_for_status()
            return r.json()
        except Exception as e:
            return {"error": "backend", "detail": str(e)}

    def close(self):
        try:
            if self._client:
                self._client.close()
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Provider
# ---------------------------------------------------------------------------

class DaimonMemoryProvider(MemoryProvider):
    """daimon-memory as a Hermes external memory provider."""

    def __init__(self):
        self._client: Optional[_DaimonClient] = None
        self._endpoint = ""
        self._tenant = _DEFAULT_TENANT
        self._namespace = _DEFAULT_NAMESPACE
        self._session_id = ""
        # background recall plumbing
        self._prefetch_lock = threading.Lock()
        self._prefetch_result = ""
        self._prefetch_thread: Optional[threading.Thread] = None
        # track in-flight write threads so on_session_end/shutdown can drain
        self._write_threads: List[threading.Thread] = []

    @property
    def name(self) -> str:
        return "daimon"

    # -- lifecycle -----------------------------------------------------------

    def is_available(self) -> bool:
        # No network — just config presence (per ABC contract).
        return bool(os.environ.get("DAIMON_ENDPOINT"))

    def initialize(self, session_id: str, **kwargs) -> None:
        self._session_id = session_id or ""
        self._endpoint = os.environ.get("DAIMON_ENDPOINT", "").rstrip("/")
        self._tenant = os.environ.get("DAIMON_TENANT", _DEFAULT_TENANT)
        self._namespace = os.environ.get("DAIMON_NAMESPACE", _DEFAULT_NAMESPACE)
        api_key = os.environ.get("DAIMON_API_KEY", "")
        if not self._endpoint:
            logger.warning("daimon: DAIMON_ENDPOINT unset; provider inert")
            return
        self._client = _DaimonClient(self._endpoint, self._tenant, api_key)
        # health check is best-effort + non-fatal
        if not self._client.healthy():
            logger.warning("daimon: backend at %s not reachable yet (recall/capture best-effort)", self._endpoint)

    def system_prompt_block(self) -> str:
        if not self._client:
            return ""
        return (
            "daimon-memory (shared cross-tool memory) is active. Relevant memories are "
            "recalled automatically and shown in <memory-context>. Use `daimon_remember` "
            "to persist durable, typed knowledge (decisions, runbooks, lessons); use "
            "`daimon_recall` for explicit lookups."
        )

    # -- recall --------------------------------------------------------------

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        # Return the background-fetched result if ready (don't block the turn).
        if self._prefetch_thread and self._prefetch_thread.is_alive():
            self._prefetch_thread.join(timeout=3.0)
        with self._prefetch_lock:
            result = self._prefetch_result
            self._prefetch_result = ""
        return result

    def queue_prefetch(self, query: str, *, session_id: str = "") -> None:
        if not self._client or not query.strip():
            return

        def _work():
            hits = self._client.recall(query)
            with self._prefetch_lock:
                self._prefetch_result = self._format_hits(hits)

        t = threading.Thread(target=_work, daemon=True)
        self._prefetch_thread = t
        t.start()

    @staticmethod
    def _format_hits(hits: List[dict]) -> str:
        if not hits:
            return ""
        lines = ["## Daimon Memory (shared, recalled)"]
        for h in hits:
            kind = h.get("kind", "")
            title = h.get("title", "")
            abstract = (h.get("abstract") or "").strip().replace("\n", " ")
            if len(abstract) > 240:
                abstract = abstract[:240] + "…"
            uri = h.get("uri", "")
            lines.append(f"- ({kind}) {title} — {abstract} [{uri}]")
        return "\n".join(lines)

    # -- capture: CURATED only, never raw turns ------------------------------

    def sync_turn(self, user_content, assistant_content, *, session_id="", messages=None) -> None:
        # Intentional no-op: daimon-memory does NOT ingest raw conversation turns.
        # Capture is curated — via on_memory_write (mirroring Hermes's own memory
        # writes) and the explicit daimon_remember tool. This keeps our LLM
        # requirement at zero (no extraction VLM on the daimon side).
        return

    def on_memory_write(self, action, target, content, metadata=None) -> None:
        """Mirror Hermes's built-in curated memory writes into daimon-memory."""
        if not self._client or action not in ("add", "replace"):
            return
        text = (content or "").strip()
        if not text:
            return
        title = text.splitlines()[0][:120]
        payload = {
            "kind": "agent_lesson",
            "namespace": f"hermes-private/{ 'user' if target == 'user' else 'memory' }",
            "title": title,
            "body": text,
            "fields": {"lesson": text},
            "tags": ["hermes", f"mirror:{target}"],
            "importance": 40,
        }
        self._spawn_write(payload)

    def _spawn_write(self, payload: dict) -> None:
        def _work():
            res = self._client.store(payload)
            if res.get("error"):
                logger.debug("daimon mirror write skipped: %s", res)

        t = threading.Thread(target=_work, daemon=True)
        self._write_threads.append(t)
        t.start()

    # -- tools ---------------------------------------------------------------

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        if not self._client:
            return []
        return [DAIMON_REMEMBER_SCHEMA, DAIMON_RECALL_SCHEMA, DAIMON_READ_SCHEMA]

    def handle_tool_call(self, tool_name: str, args: Dict[str, Any], **kwargs) -> str:
        if not self._client:
            return json.dumps({"error": "daimon endpoint not configured"})
        if tool_name == "daimon_remember":
            payload = {
                "kind": args.get("kind", ""),
                "namespace": args.get("namespace") or self._namespace,
                "title": args.get("title", ""),
                "body": args.get("body", ""),
                "fields": args.get("fields", {}) or {},
                "tags": args.get("tags", []) or [],
                "importance": int(args.get("importance", 0) or 0),
            }
            return json.dumps(self._client.store(payload))
        if tool_name == "daimon_recall":
            hits = self._client.recall(
                args.get("query", ""), args.get("kind"), int(args.get("limit", 10) or 10)
            )
            return json.dumps({"hits": hits})
        if tool_name == "daimon_read":
            return json.dumps(self._client.read(args.get("uri", "")))
        return json.dumps({"error": f"unknown tool: {tool_name}"})

    # -- session lifecycle ---------------------------------------------------

    def on_session_end(self, messages: List[Dict[str, Any]]) -> None:
        # No extraction pipeline; just drain pending writes.
        self._drain_writes(timeout=5.0)

    def _drain_writes(self, timeout: float) -> None:
        for t in list(self._write_threads):
            if t.is_alive():
                t.join(timeout=timeout)
        self._write_threads = [t for t in self._write_threads if t.is_alive()]

    def shutdown(self) -> None:
        self._drain_writes(timeout=5.0)
        if self._client:
            self._client.close()

    # -- setup (env-only config; save_config stays no-op) --------------------

    def get_config_schema(self) -> List[Dict[str, Any]]:
        return [
            {
                "key": "endpoint",
                "description": "daimon-memory base URL (e.g. http://10.100.30.27)",
                "required": True,
                "env_var": "DAIMON_ENDPOINT",
            },
            {
                "key": "tenant",
                "description": "Tenant UUID",
                "required": False,
                "default": _DEFAULT_TENANT,
                "env_var": "DAIMON_TENANT",
            },
            {
                "key": "namespace",
                "description": "Default capture namespace",
                "required": False,
                "default": _DEFAULT_NAMESPACE,
                "env_var": "DAIMON_NAMESPACE",
            },
            {
                "key": "api_key",
                "description": "Bearer token (only if daimon-mcp auth is enabled)",
                "required": False,
                "secret": True,
                "env_var": "DAIMON_API_KEY",
            },
        ]


def register(ctx) -> None:
    """Plugin entry point — discovered by Hermes's memory-provider loader."""
    ctx.register_memory_provider(DaimonMemoryProvider())
