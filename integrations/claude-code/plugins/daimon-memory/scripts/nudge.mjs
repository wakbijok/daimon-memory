#!/usr/bin/env node
// UserPromptSubmit nudge hook. Reads the PREVIOUS assistant turn from the transcript, scans
// it for an uncaptured save-signal (and whether a save tool already ran), and, per the Save
// Discipline, nudges the agent to call the exact guided tool. Cadence-gated, best-effort.
// Runs alongside auto-recall.mjs (both inject additionalContext; Claude Code concatenates).
import { readFileSync } from "node:fs";
import { readStdin } from "./lib/daimon.mjs";
import { NUDGE_ON, SAVE_TOOLS, scanSignal, loadState, saveState, decide } from "./nudge-lib.mjs";

const input = await readStdin();
if (!NUDGE_ON) process.exit(0);
const sessionId = input.session_id || input.sessionId || "default";
const tpath = input.transcript_path || input.transcriptPath || "";

// Collect the most recent contiguous block of assistant messages (the previous turn):
// its text and any tool_use names, scanning the transcript jsonl from the end.
function lastAssistantTurn(path) {
  let text = "";
  const tools = [];
  if (!path) return { text, tools };
  try {
    const lines = readFileSync(path, "utf8").trim().split("\n");
    for (let i = lines.length - 1; i >= 0; i--) {
      let obj;
      try { obj = JSON.parse(lines[i]); } catch { continue; }
      const role = obj.message?.role || obj.role || obj.type;
      if (role === "user") break;            // hit the previous user msg -> turn boundary
      if (role !== "assistant") continue;
      const content = obj.message?.content ?? obj.content ?? [];
      if (Array.isArray(content)) {
        for (const c of content) {
          if (c?.type === "text") text += " " + (c.text || "");
          else if (c?.type === "tool_use") tools.push(c.name || "");
          else if (typeof c === "string") text += " " + c;
        }
      } else if (typeof content === "string") {
        text += " " + content;
      }
    }
  } catch { /* ignore */ }
  return { text, tools };
}

const { text, tools } = lastAssistantTurn(tpath);
const didSave = tools.some((t) => SAVE_TOOLS.has(t));
const signal = scanSignal(text);
const state = loadState(sessionId);
const nudge = decide(state, { signal, didSave });
saveState(sessionId, state);

if (nudge && nudge.trim()) {
  process.stdout.write(JSON.stringify({
    hookSpecificOutput: { hookEventName: "UserPromptSubmit", additionalContext: nudge },
  }));
}
process.exit(0);
