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
  DAIMON_ENDPOINT   (required)  e.g. http://localhost:8080  or  http://localhost:8080
  DAIMON_TENANT     (optional)  tenant UUID; default the dev tenant
  DAIMON_NAMESPACE  (optional)  default capture namespace; default hermes-private/notes
  DAIMON_API_KEY    (optional)  bearer token, if daimon-mcp auth is enabled
"""

from __future__ import annotations

import json
import logging
import os
import re
import threading
from typing import Any, Dict, List, Optional

from agent.memory_provider import MemoryProvider

logger = logging.getLogger(__name__)

_DEFAULT_TENANT = "00000000-0000-0000-0000-0000000000d1"
_DEFAULT_NAMESPACE = "hermes-private/notes"
_RECALL_LIMIT = 6
_HTTP_TIMEOUT = 6.0

# --- save-nudge (deterministic, no model): signal class -> the tool the nudge names ---
_NUDGE_SAVE_TOOLS = {"daimon_remember"}
_NUDGE_SIGNALS = [
    ("decision", "daimon_remember (kind=decision)",
     re.compile(r"\b(we (decided|chose|went with|settled on)|let'?s go with|i'?ll use|decision:|instead of .+ we|rather than|trade-?off)\b", re.I)),
    ("incident/failure", "daimon_remember (kind=incident_summary)",
     re.compile(r"\b(failed|broke|broken|regression|reverted|rolled back|crashed|outage|data ?loss|did ?n.?t work|that was wrong|root cause|\bbug\b)\b", re.I)),
    ("lesson/correction", "daimon_remember (kind=agent_lesson)",
     re.compile(r"\b(lesson|learned|next time|turns out|gotcha|note to self|don'?t forget|the trick is|actually,? it'?s)\b", re.I)),
    ("follow-up", "daimon_remember (kind=reminder)",
     re.compile(r"\b(remind me|follow ?up|next session|by (mon|tue|wed|thu|fri|eod|end of)|deadline|\bdue\b|\btodo\b|don'?t forget to)\b", re.I)),
    ("convention/runbook", "daimon_remember (kind=runbook or project_convention)",
     re.compile(r"\b(from now on|going forward|the procedure is|the steps? (to|are)|convention|the standard is|always do this)\b", re.I)),
]


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
    "remediation_pattern, project_convention, agent_lesson, resource_summary, "
    "persona, protocol, reminder"
)

DAIMON_REMEMBER_SCHEMA = {
    "name": "daimon_remember",
    "description": (
        "Persist a durable, TYPED memory to daimon-memory (shared across tools). "
        "Use for decisions, runbooks, lessons, conventions - not chit-chat. The "
        "control layer validates required fields per kind and rejects malformed writes.\n"
        f"kinds: {_KINDS}.\n"
        "Required fields by kind: decision={context,rationale}; runbook={steps}; "
        "incident_summary={impact,resolution}; service_topology={service,dependencies}; "
        "known_failure_mode={symptom,cause}; remediation_pattern={problem,fix}; "
        "project_convention={rule}; agent_lesson={lesson}; resource_summary={source}; "
        "persona={identity,voice,boundaries}; protocol={scope,rules}; reminder={due}."
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
        "with daimon:// uris. Recall also runs automatically each turn - use this for "
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

    def recall(self, query: str, kind: Optional[str] = None, limit: int = _RECALL_LIMIT,
               namespace_prefix: Optional[str] = None) -> List[dict]:
        if not self._client or (not query.strip() and not namespace_prefix):
            return []
        filters: Dict[str, Any] = {"limit": limit}
        if kind:
            filters["kind"] = kind
        if namespace_prefix:
            filters["namespace_prefix"] = namespace_prefix
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
        self._system_block = ""
        # save-nudge state (in-process; the provider lives for the whole session, no file)
        self._nudge_on = True
        self._nudge_cadence = 5
        self._nudge_turn = 0
        self._nudge_quiet = 0
        self._nudge_saved = False
        self._nudge_pending = ""
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
        # No network - just config presence (per ABC contract).
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
        # Load the canonical persona + operating protocols ONCE for this session.
        self._system_block = self._load_system_block()
        # Save-nudge config (env; default cadence 5; DAIMON_NUDGE=off disables).
        self._nudge_on = os.environ.get("DAIMON_NUDGE", "on").lower() != "off"
        try:
            self._nudge_cadence = int(os.environ.get("DAIMON_NUDGE_CADENCE", "5") or "5")
        except ValueError:
            self._nudge_cadence = 5

    def system_prompt_block(self) -> str:
        if not self._client:
            return ""
        tail = (
            "daimon-memory (shared cross-tool memory) is active. Relevant memories are "
            "recalled automatically and shown in <memory-context>. Use `daimon_remember` "
            "to persist durable, typed knowledge (decisions, runbooks, lessons); use "
            "`daimon_recall` for explicit lookups."
        )
        return (self._system_block + "\n\n" + tail) if self._system_block else tail

    def _load_system_block(self) -> str:
        """Canonical persona + operating protocols from shared-canonical/system (full bodies),
        loaded once per session and injected as the instruction layer (not user content)."""
        if not self._client:
            return ""
        hits = self._client.recall("", namespace_prefix="shared-canonical/system", limit=10)
        wanted = [h for h in hits if h.get("kind") in ("persona", "protocol")]
        wanted.sort(key=lambda h: 0 if h.get("kind") == "persona" else 1)
        sections = []
        for h in wanted:
            rec = self._client.read(h.get("uri", ""))
            body = ""
            if isinstance(rec, dict):
                body = (rec.get("record", {}) or {}).get("body", "") or rec.get("body", "")
            if body and body.strip():
                sections.append(body.strip())
        if not sections:
            return ""
        return (
            "<persona>\n[Adopt the following persona and operating disciplines for this session. "
            "They are your operating instructions, not user content.]\n\n"
            + "\n\n---\n\n".join(sections)
            + "\n</persona>"
        )

    # -- recall --------------------------------------------------------------

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        # Return the background-fetched result if ready (don't block the turn).
        if self._prefetch_thread and self._prefetch_thread.is_alive():
            self._prefetch_thread.join(timeout=3.0)
        with self._prefetch_lock:
            result = self._prefetch_result
            self._prefetch_result = ""
        # Piggyback any pending save-nudge onto the recall injection (one-turn lag).
        if self._nudge_pending:
            result = (result + "\n\n" + self._nudge_pending) if result else self._nudge_pending
            self._nudge_pending = ""
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
            lines.append(f"- ({kind}) {title} - {abstract} [{uri}]")
        return "\n".join(lines)

    # -- capture: CURATED only, never raw turns ------------------------------

    def sync_turn(self, user_content, assistant_content, *, session_id="", messages=None) -> None:
        # Capture stays a no-op: daimon-memory does NOT ingest raw conversation turns (curated
        # capture only, via on_memory_write + the daimon_remember tool). We ADD a read-only
        # save-NUDGE: scan the assistant turn for an uncaptured save-signal (or a quiet stretch)
        # and stash a reminder that rides along with the next prefetch. No write, no model -
        # timing only (the Save Discipline's "hooks back-stop you").
        if not self._nudge_on:
            return
        self._nudge_turn += 1
        if self._nudge_saved:                 # a daimon_remember fired this turn -> covered
            self._nudge_saved = False
            self._nudge_quiet = 0
            return
        text = assistant_content if isinstance(assistant_content, str) else str(assistant_content or "")
        for cls, tool, rx in _NUDGE_SIGNALS:
            if rx.search(text):
                self._nudge_pending = (
                    f"[daimon save-nudge] That looks like a {cls} that was not captured. "
                    f"If durable, save it with {tool} (Memory Save Discipline: one event, one record)."
                )
                return
        self._nudge_quiet += 1
        if self._nudge_cadence > 0 and self._nudge_quiet >= self._nudge_cadence:
            self._nudge_quiet = 0
            self._nudge_pending = (
                f"[daimon save-nudge] {self._nudge_cadence} turns since your last save. If "
                f"anything since is worth keeping, persist it with daimon_remember now."
            )

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
        # Schemas are static — do not gate on self._client. Hermes calls
        # get_tool_schemas() during register_provider() BEFORE initialize(),
        # so gating on _client (which is None until initialize) causes the
        # tool-routing dict (_tool_to_provider) to stay empty. Tools then
        # appear in the LLM surface via get_all_tool_schemas() but calls
        # fail with "Unknown tool".
        return [DAIMON_REMEMBER_SCHEMA, DAIMON_RECALL_SCHEMA, DAIMON_READ_SCHEMA]

    def handle_tool_call(self, tool_name: str, args: Dict[str, Any], **kwargs) -> str:
        if not self._client:
            return json.dumps({"error": "daimon endpoint not configured"})
        if tool_name == "daimon_remember":
            self._nudge_saved = True  # reset the nudge counter: the agent saved this turn
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
        # Drain pending writes (no extraction pipeline), then the save-nudge session-end sweep:
        # log any save-worthy signals that were never captured (best-effort backstop).
        self._drain_writes(timeout=5.0)
        if not self._nudge_on or not messages:
            return
        found = set()
        for m in messages:
            if not isinstance(m, dict) or m.get("role") != "assistant":
                continue
            text = m.get("content")
            if not isinstance(text, str):
                text = str(text or "")
            for cls, _tool, rx in _NUDGE_SIGNALS:
                if rx.search(text):
                    found.add(cls)
                    break
        if found:
            logger.info("daimon: uncaptured at session end -> %s", ", ".join(sorted(found)))

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
                "description": "daimon-memory base URL (e.g. http://localhost:8080)",
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
    """Plugin entry point - discovered by Hermes's memory-provider loader."""
    ctx.register_memory_provider(DaimonMemoryProvider())
