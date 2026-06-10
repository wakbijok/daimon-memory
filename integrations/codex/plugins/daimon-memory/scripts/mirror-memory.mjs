#!/usr/bin/env node
// SessionStart side-effect hook. Codex has no SessionEnd and writes its native memory
// ASYNCHRONOUSLY hours after a thread idles, so we mirror the accumulated store on the NEXT
// boot. Reads Codex native memory (SQLite at ~/.codex/memories_1.sqlite -> stage1_outputs)
// and mirrors each entry into daimon as an agent_lesson in agent/lessons. Injects
// nothing into context. Best-effort: always exits 0. Dedup: a client watermark on
// generated_at (skip unchanged rows) + the server's content-hash (idempotent re-store).
// Zero npm deps: uses the built-in node:sqlite, opened read-only (never blocks Codex's writer).
import { readStdin, store } from "./lib/daimon.mjs";
import { readFileSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

await readStdin();

const CODEX_HOME = process.env.CODEX_HOME || join(homedir(), ".codex");
const DB = join(CODEX_HOME, "memories_1.sqlite");
const STATE = join(tmpdir(), "daimon-codex-mirror.json");

function loadWatermark() {
  try { return JSON.parse(readFileSync(STATE, "utf8")).watermark ?? 0; }
  catch { return 0; }
}
function saveWatermark(w) {
  try { writeFileSync(STATE, JSON.stringify({ watermark: w })); } catch { /* best-effort */ }
}

let rows = [];
try {
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(DB, { readOnly: true });
  rows = db
    .prepare(
      `SELECT thread_id, raw_memory, rollout_summary, rollout_slug, generated_at
         FROM stage1_outputs
        WHERE raw_memory IS NOT NULL AND TRIM(raw_memory) <> ''
        ORDER BY generated_at ASC`,
    )
    .all();
  db.close();
} catch {
  process.exit(0); // DB missing/empty/locked, or node:sqlite unavailable: nothing to mirror
}

const since = loadWatermark();
let maxSeen = since;

for (const r of rows) {
  const gen = Number(r.generated_at ?? 0);
  // Strictly-less-than: rows SHARING the watermark timestamp are re-posted each session,
  // because a failure on the second of two same-timestamp rows would otherwise be skipped
  // forever. The server's content-sha dedup makes the re-store an idempotent no-op.
  if (gen < since) continue; // already mirrored (watermark fast-path)
  const text = String(r.raw_memory || "").trim();
  if (!text) continue;

  const summary = String(r.rollout_summary || "").trim();
  const firstLine = summary.split(/[.\n]/)[0].trim();
  const title = (firstLine || r.rollout_slug || `codex-memory ${String(r.thread_id).slice(0, 12)}`)
    .slice(0, 120);

  // Advance the watermark ONLY on confirmed store (true = stored or validation-reject;
  // false = network failure). Stop at the first failure so the watermark stays contiguous -
  // advancing past a failed row would permanently skip it on every future session.
  const ok = await store({
    kind: "agent_lesson",
    namespace: "agent/lessons",
    title,
    body: text,
    fields: { lesson: text },
    tags: ["codex-memory", "mirror"],
    importance: 30,
  });
  if (!ok) break;
  if (gen > maxSeen) maxSeen = gen;
}

saveWatermark(maxSeen);
process.exit(0);
