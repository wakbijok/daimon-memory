// Local, plugin-owned store for the pre-compaction snapshot (compaction-only continuity).
// PreCompact writes the volatile working context here; SessionStart (source=compact) reads,
// injects, then clears it. Stays on disk and NEVER goes to the daimon backend: this is
// ephemeral session scratch (branch/cwd/dirty files), not curated shared memory, and would
// pollute semantic recall if the indexer embedded it. Keyed by session_id (compaction stays
// within one session).
import { readFileSync, writeFileSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";

function statePath(sessionId) {
  const safe = String(sessionId || "default").replace(/[^a-zA-Z0-9_-]/g, "_");
  const base = process.env.XDG_STATE_HOME || join(homedir(), ".local", "state");
  try {
    const dir = join(base, "daimon-memory", "precompact");
    mkdirSync(dir, { recursive: true });
    return join(dir, `${safe}.json`);
  } catch {
    return join(tmpdir(), `daimon-precompact-${safe}.json`);
  }
}

export function savePrecompact(sessionId, data) {
  try { writeFileSync(statePath(sessionId), JSON.stringify(data)); } catch { /* best-effort */ }
}
export function loadPrecompact(sessionId) {
  try { return JSON.parse(readFileSync(statePath(sessionId), "utf8")); } catch { return null; }
}
export function clearPrecompact(sessionId) {
  try { rmSync(statePath(sessionId), { force: true }); } catch { /* best-effort */ }
}
