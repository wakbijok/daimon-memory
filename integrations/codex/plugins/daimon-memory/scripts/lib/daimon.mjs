// Shared config + HTTP helpers for the daimon-memory Codex plugin.
// Config resolution: env (DAIMON_ENDPOINT/DAIMON_TENANT) -> daimon.config.json written
// by the installer (Codex does not reliably pass env to hook subprocesses) -> defaults.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

function fileConfig() {
  try {
    const here = dirname(fileURLToPath(import.meta.url));
    return JSON.parse(readFileSync(join(here, "daimon.config.json"), "utf8"));
  } catch { return {}; }
}
const _c = fileConfig();
export const ENDPOINT = (process.env.DAIMON_ENDPOINT || _c.endpoint || "http://localhost:8080").replace(/\/+$/, "");
export const TENANT = process.env.DAIMON_TENANT || _c.tenant || "00000000-0000-0000-0000-0000000000d1";

export async function readStdin() {
  const chunks = [];
  try { for await (const c of process.stdin) chunks.push(c); } catch { /* ignore */ }
  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) return {};
  try { return JSON.parse(raw); } catch { return {}; }
}

// POST /v1/recall with a hard timeout. Returns hits[] or [] on ANY failure.
// Empty query is allowed: the server returns recent, high-importance records.
export async function recall(query, { limit = 6, kind = null } = {}) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 6000);
  try {
    const filters = { limit };
    if (kind) filters.kind = kind;
    const r = await fetch(`${ENDPOINT}/v1/recall`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-daimon-tenant": TENANT },
      body: JSON.stringify({ query: query || "", filters }),
      signal: ctrl.signal,
    });
    if (!r.ok) return [];
    const j = await r.json();
    return Array.isArray(j.hits) ? j.hits : [];
  } catch { return []; }
  finally { clearTimeout(timer); }
}

export function formatHits(hits, heading) {
  if (!hits || !hits.length) return "";
  const lines = [heading];
  for (const h of hits) {
    const abs = String(h.abstract || "").replace(/\s+/g, " ").slice(0, 240);
    lines.push(`- (${h.kind}) ${h.title}: ${abs} [${h.uri}]`);
  }
  return lines.join("\n");
}

// Codex UserPromptSubmit/SessionStart use the same hookSpecificOutput.additionalContext
// protocol as Claude Code. Always exit 0 (best-effort).
export function injectAndExit(eventName, text) {
  if (text && text.trim()) {
    process.stdout.write(JSON.stringify({
      hookSpecificOutput: { hookEventName: eventName, additionalContext: text },
    }));
  }
  process.exit(0);
}
