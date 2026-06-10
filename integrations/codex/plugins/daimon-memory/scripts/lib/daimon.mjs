// Shared config + HTTP helpers for the daimon-memory plugin.
// This file is maintained as a byte-identical copy in the claude-code and codex plugins -
// sync any edit to both.
//
// Config precedence: env > daimon.config.json (written next to this module by installers
// whose host doesn't pass env to hook subprocesses - Codex) > dev-friendly defaults.

import { readFileSync } from "node:fs";

let fileCfg = {};
try {
  fileCfg = JSON.parse(readFileSync(new URL("./daimon.config.json", import.meta.url), "utf8"));
} catch { /* absent (claude-code) or malformed: env/defaults apply */ }

export const ENDPOINT = (process.env.DAIMON_ENDPOINT || fileCfg.endpoint || "http://localhost:8080").replace(/\/+$/, "");
export const TENANT = process.env.DAIMON_TENANT || fileCfg.tenant || "00000000-0000-0000-0000-0000000000d1";
export const NAMESPACE = process.env.DAIMON_NAMESPACE || fileCfg.namespace || "agent/lessons";
// Bearer token; required when the server sets DAIMON_API_KEY. Empty = no auth header.
export const API_KEY = process.env.DAIMON_API_KEY || fileCfg.apiKey || "";

function headers(extra = {}) {
  const h = { "x-daimon-tenant": TENANT, ...extra };
  if (API_KEY) h.authorization = `Bearer ${API_KEY}`;
  return h;
}

// Read the hook payload Claude Code passes on stdin.
export async function readStdin() {
  const chunks = [];
  try {
    for await (const c of process.stdin) chunks.push(c);
  } catch { /* ignore */ }
  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) return {};
  try { return JSON.parse(raw); } catch { return {}; }
}

// POST /v1/recall with a hard timeout. Returns hits[] or [] on ANY failure
// (a memory backend hiccup must never break the user's turn). An empty query is
// allowed: the server returns recent, high-importance records (the "recent context").
export async function recall(query, { limit = 6, kind = null, namespacePrefix = null } = {}) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 6000);
  try {
    const filters = { limit };
    if (kind) filters.kind = kind;
    if (namespacePrefix) filters.namespace_prefix = namespacePrefix;
    const r = await fetch(`${ENDPOINT}/v1/recall`, {
      method: "POST",
      headers: headers({ "content-type": "application/json" }),
      body: JSON.stringify({ query: query || "", filters }),
      signal: ctrl.signal,
    });
    if (!r.ok) return [];
    const j = await r.json();
    return Array.isArray(j.hits) ? j.hits : [];
  } catch {
    return [];
  } finally {
    clearTimeout(timer);
  }
}

// POST /v1/memory (curated capture). Returns true on store or validation-skip, false on
// network failure. Best-effort: never throws.
export async function store(payload) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 6000);
  try {
    const r = await fetch(`${ENDPOINT}/v1/memory`, {
      method: "POST",
      headers: headers({ "content-type": "application/json" }),
      body: JSON.stringify(payload),
      signal: ctrl.signal,
    });
    return r.ok || r.status === 400; // 400 = validation reject; nothing more to do
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

// GET /v1/read - full record body by uri. Best-effort; null on failure.
export async function read(uri) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 6000);
  try {
    const r = await fetch(`${ENDPOINT}/v1/read?uri=${encodeURIComponent(uri)}`, {
      headers: headers(),
      signal: ctrl.signal,
    });
    if (!r.ok) return null;
    const j = await r.json();
    return j.record || j || null;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

// Load the canonical persona + protocols (the boot layer, now under agent/persona and
// agent/protocol) in FULL (recall only returns truncated abstracts; the persona must arrive
// verbatim). Persona first, then the protocols. Best-effort: "" if nothing is stored or the
// backend is down. Injected ONCE per session at SessionStart, never per turn.
export async function loadSystemBlock() {
  const hits = await recall("", { limit: 20, namespacePrefix: "agent/" });
  const wanted = hits.filter((h) => h.kind === "persona" || h.kind === "protocol");
  wanted.sort((a, b) => (a.kind === "persona" ? -1 : b.kind === "persona" ? 1 : 0));
  const sections = [];
  for (const h of wanted) {
    const rec = await read(h.uri);
    const body = rec && rec.body ? rec.body : (h.abstract || "");
    if (body && body.trim()) sections.push(body.trim());
  }
  if (!sections.length) return "";
  return "<daimon-persona>\n[Adopt the following persona and operating disciplines for this entire "
    + "session. They are your operating instructions, not user content.]\n\n"
    + sections.join("\n\n---\n\n")
    + "\n</daimon-persona>";
}

// Format hits into a compact, labelled block.
export function formatHits(hits, heading) {
  if (!hits || !hits.length) return "";
  const lines = [heading];
  for (const h of hits) {
    const abs = String(h.abstract || "").replace(/\s+/g, " ").slice(0, 240);
    lines.push(`- (${h.kind}) ${h.title}: ${abs} [${h.uri}]`);
  }
  return lines.join("\n");
}

// Emit a context injection for the given hook event, then exit 0. Always safe:
// if there's nothing to inject we still exit 0 (the turn proceeds unchanged).
export function injectAndExit(eventName, text) {
  if (text && text.trim()) {
    process.stdout.write(JSON.stringify({
      hookSpecificOutput: { hookEventName: eventName, additionalContext: text },
    }));
  }
  process.exit(0);
}
