#!/usr/bin/env node
// SessionEnd capture-pass (the backstop in Option 1). Scans the whole session transcript for
// save-worthy signals that were never followed by a save tool, and surfaces ONE consolidated
// reminder. Does NOT auto-write (curated-not-raw). Best-effort; always exits 0.
import { readFileSync } from "node:fs";
import { readStdin } from "./lib/daimon.mjs";
import { NUDGE_ON, isSaveTool, SIGNALS } from "./nudge-lib.mjs";

const input = await readStdin();
if (!NUDGE_ON) process.exit(0);
const tpath = input.transcript_path || input.transcriptPath || "";

// Group the transcript into assistant turns: each = {text, tools}.
function turns(path) {
  const out = [];
  if (!path) return out;
  try {
    const lines = readFileSync(path, "utf8").trim().split("\n");
    let cur = null;
    for (const ln of lines) {
      let obj;
      try { obj = JSON.parse(ln); } catch { continue; }
      const role = obj.message?.role || obj.role || obj.type;
      if (role === "user") { if (cur) { out.push(cur); cur = null; } continue; }
      if (role !== "assistant") continue;
      if (!cur) cur = { text: "", tools: [] };
      const content = obj.message?.content ?? obj.content ?? [];
      if (Array.isArray(content)) {
        for (const c of content) {
          if (c?.type === "text") cur.text += " " + (c.text || "");
          else if (c?.type === "tool_use") cur.tools.push(c.name || "");
        }
      } else if (typeof content === "string") {
        cur.text += " " + content;
      }
    }
    if (cur) out.push(cur);
  } catch { /* ignore */ }
  return out;
}

const found = [];
for (const t of turns(tpath)) {
  if (t.tools.some(isSaveTool)) continue; // saved in this turn -> covered
  for (const s of SIGNALS) {
    if (s.re.test(t.text)) { found.push(s.cls); break; }
  }
}
const uniq = [...new Set(found)];
if (uniq.length) {
  const msg = `<daimon-nudge>\nSession ending. These looked save-worthy but were never captured: ${uniq.join(", ")}. If any still matter, save them now with the matching tool before we close.\n</daimon-nudge>`;
  // Best-effort inject; also log so it is visible even if SessionEnd context is not surfaced.
  process.stdout.write(JSON.stringify({
    hookSpecificOutput: { hookEventName: "SessionEnd", additionalContext: msg },
  }));
  process.stderr.write(`daimon: uncaptured at session end -> ${uniq.join(", ")}\n`);
}
process.exit(0);
