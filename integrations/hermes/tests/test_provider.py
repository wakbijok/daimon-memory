"""Regression test for the daimon Hermes memory provider.

Guards the turn-1 cold-start fix: the provider MUST warm recall for the current
turn in on_turn_start(), because Hermes' turn prologue calls on_turn_start(turn,
msg) immediately before prefetch_all() -> prefetch(). Without it, the only warming
is run_agent's end-of-turn queue_prefetch (for the NEXT turn), so the first turn of
a session has an empty <memory-context>.

Run (from integrations/hermes): python3 tests/test_provider.py
(No backend needed — the daimon HTTP client is faked.)
"""
import os
import sys
import types

_HERE = os.path.dirname(os.path.abspath(__file__))

# Stub the Hermes base so the provider imports without the full agent package.
_agent = types.ModuleType("agent")
_mp = types.ModuleType("agent.memory_provider")


class MemoryProvider:  # matches the real ABC's no-op optional hooks
    def on_turn_start(self, turn_number, message, **kwargs):
        pass


_mp.MemoryProvider = MemoryProvider
sys.modules.setdefault("agent", _agent)
sys.modules.setdefault("agent.memory_provider", _mp)

sys.path.insert(0, os.path.dirname(_HERE))  # package root (parent of tests/)
import daimon_hermes as daimon  # noqa: E402


class _FakeClient:
    def __init__(self):
        self.recall_queries = []
        self.stores = []

    def recall(self, query, kind=None, limit=6, namespace_prefix=None):
        self.recall_queries.append(query)
        return [{
            "kind": "decision",
            "title": "Backup choice",
            "abstract": "chose Velero CSI-SDM to Garage",
            "uri": "daimon://resources/homelab/backup/decision/abc",
        }]

    def store(self, payload):
        self.stores.append(payload)
        return {"ok": True}


def test_on_turn_start_warms_first_turn():
    p = daimon.DaimonMemoryProvider()
    p._client = _FakeClient()
    p._session_id = "sess-1"

    # Turn-1 prologue order: on_turn_start THEN prefetch (see turn_context.py).
    p.on_turn_start(1, "what backup did we pick?")
    ctx = p.prefetch("what backup did we pick?")

    assert ctx and "Backup choice" in ctx and "daimon://" in ctx, (
        f"turn-1 cold-start: prefetch empty after on_turn_start; got={ctx!r}"
    )
    assert p._client.recall_queries == ["what backup did we pick?"], (
        f"on_turn_start must warm recall with the CURRENT message; "
        f"got={p._client.recall_queries!r}"
    )


def test_empty_message_does_not_warm():
    p = daimon.DaimonMemoryProvider()
    p._client = _FakeClient()
    p.on_turn_start(1, "   ")
    assert p._client.recall_queries == [], "blank message must not trigger recall"


def test_get_tool_schemas_without_client():
    # Hermes calls get_tool_schemas() during register_provider(), BEFORE initialize() sets
    # _client. It must still return all 3 tool schemas or tool routing breaks ("Unknown tool").
    p = daimon.DaimonMemoryProvider()
    assert p._client is None
    names = {s["name"] for s in p.get_tool_schemas()}
    assert names == {"daimon_remember", "daimon_recall", "daimon_read"}, names


def test_prefetch_single_consume_and_nudge():
    p = daimon.DaimonMemoryProvider()
    p._client = _FakeClient()
    p.on_turn_start(1, "q")
    assert "Backup choice" in p.prefetch("q")
    assert p.prefetch("q") == "", "prefetch must single-consume (stash cleared on read)"
    # save-nudge piggybacks onto one prefetch, then clears
    p._nudge_pending = "[daimon save-nudge] test"
    out = p.prefetch("q")
    assert "save-nudge" in out and p._nudge_pending == ""
    assert p.prefetch("q") == "", "nudge must not repeat"


def test_on_memory_write_uses_subject_keyed_namespaces():
    # daimon convention: a write ABOUT the user -> user/; the agent's own note -> agent/.
    # Never the old hermes-private/*.
    p = daimon.DaimonMemoryProvider()
    fc = _FakeClient()
    p._client = fc
    p.on_memory_write("add", "user", "user prefers dark mode")
    p.on_memory_write("add", "memory", "agent learned a deploy gotcha")
    p._drain_writes(timeout=3.0)
    namespaces = sorted(s["namespace"] for s in fc.stores)
    assert namespaces == ["agent/lessons", "user/preferences"], namespaces
    assert all(not n.startswith("hermes-private") for n in namespaces), namespaces


if __name__ == "__main__":
    test_on_turn_start_warms_first_turn()
    test_empty_message_does_not_warm()
    test_get_tool_schemas_without_client()
    test_prefetch_single_consume_and_nudge()
    test_on_memory_write_uses_subject_keyed_namespaces()
    print("PASS: daimon provider (on_turn_start, get_tool_schemas, single-consume, nudge, namespaces)")
