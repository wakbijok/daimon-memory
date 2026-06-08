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

// Collect the most recent contiguous block of assistant/tool entries (the previous turn):
// its text + any tool/function-call names. Handles BOTH the Claude Code transcript
// (message.content[] with tool_use blocks) and the Codex rollout (payload.content[] with
// input_text/output_text, plus separate function_call entries).
function lastAssistantTurn(path) {
  let text = "";
  const tools = [];
  if (!path) return { text, tools };
  let lines;
  try { lines = readFileSync(path, "utf8").trim().split("\n"); } catch { return { text, tools }; }
  for (let i = lines.length - 1; i >= 0; i--) {
    let obj;
    try { obj = JSON.parse(lines[i]); } catch { continue; }
    const p = obj.payload && typeof obj.payload === "object" ? obj.payload : obj;
    const role = p.message?.role || p.role || obj.type || p.type;
    if (role === "user") break;                       // previous user msg -> turn boundary
    const etype = p.type || obj.type;
    if (etype === "function_call" || etype === "tool_use" || etype === "local_shell_call") {
      const name = p.name || p.function?.name || p.tool_name || "";
      if (name) tools.push(name);
      continue;
    }
    const content = p.message?.content ?? p.content ?? [];
    if (typeof content === "string") {
      text += " " + content;
    } else if (Array.isArray(content)) {
      for (const c of content) {
        if (!c) continue;
        if (typeof c === "string") text += " " + c;
        else if (c.type === "text" || c.type === "input_text" || c.type === "output_text") text += " " + (c.text || "");
        else if (c.type === "tool_use") tools.push(c.name || "");
      }
    }
  }
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
