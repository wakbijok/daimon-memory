// Session-scoped "already injected" set. This makes the per-turn auto-recall HOT and
// INCREMENTAL: it injects only records the model has not already seen this session, instead
// of re-paying tokens for memory that is already in the conversation context. Cleared on
// SessionStart, which re-fires on compaction, so anything compaction dropped is refreshed.
import { readFileSync, writeFileSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";

function statePath(sessionId) {
  const safe = String(sessionId || "default").replace(/[^a-zA-Z0-9_-]/g, "_");
  const base = process.env.XDG_STATE_HOME || join(homedir(), ".local", "state");
  try {
    const dir = join(base, "daimon-memory", "recall");
    mkdirSync(dir, { recursive: true });
    return join(dir, `${safe}.json`);
  } catch {
    return join(tmpdir(), `daimon-recall-${safe}.json`);
  }
}

export function loadInjected(sessionId) {
  try { return new Set(JSON.parse(readFileSync(statePath(sessionId), "utf8")).uris || []); }
  catch { return new Set(); }
}
export function markInjected(sessionId, uris) {
  const set = loadInjected(sessionId);
  for (const u of uris) set.add(u);
  try { writeFileSync(statePath(sessionId), JSON.stringify({ uris: [...set] })); } catch { /* best-effort */ }
}
export function clearInjected(sessionId) {
  try { rmSync(statePath(sessionId), { force: true }); } catch { /* best-effort */ }
}

// Relevance floor: drop weak semantic-only matches so we stop injecting marginally-related
// records every turn. Keyword matches (an actual term hit) are always kept. Configurable.
export const MIN_COSINE = (() => {
  const v = parseFloat(process.env.DAIMON_RECALL_MIN_COSINE ?? "");
  return Number.isFinite(v) ? v : 0.35;
})();

export function relevant(hit) {
  const s = hit.scores || {};
  if (typeof s.raw_keyword === "number" && s.raw_keyword > 0) return true; // a real term match
  if (typeof s.raw_semantic === "number") return s.raw_semantic >= MIN_COSINE;
  return true; // no score info (older server): keep
}
