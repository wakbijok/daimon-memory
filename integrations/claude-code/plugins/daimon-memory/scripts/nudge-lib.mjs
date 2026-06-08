// Deterministic save-nudge engine (NO model). Shared by the Claude Code + Codex nudge
// hooks. It scans a turn for save-signals, tracks a per-session quiet-turn counter, and
// decides whether to remind the agent to call a guided save tool. The agent still writes
// the record; this only governs TIMING (the Save Discipline's "hooks back-stop you").
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";

// --- config (per-install, env-overridable) ---
export const NUDGE_ON = String(process.env.DAIMON_NUDGE || "on").toLowerCase() !== "off";
export const CADENCE = (() => {
  const n = parseInt(process.env.DAIMON_NUDGE_CADENCE ?? "", 10);
  return Number.isFinite(n) && n >= 0 ? n : 5; // 0 disables the cadence nudge; default 5
})();

// --- save-signal classes: deterministic regex -> the exact guided tool to name ---
export const SIGNALS = [
  { cls: "decision", tool: "log_decision",
    re: /\b(we (decided|chose|went with|settled on)|let'?s go with|i'?ll use|decision:|instead of .+ we|rather than|trade-?off)\b/i },
  { cls: "incident/failure", tool: "log_incident",
    re: /\b(failed|broke|broken|regression|reverted|rolled back|crashed|outage|data ?loss|did ?n.?t work|that was wrong|root cause|\bbug\b)\b/i },
  { cls: "lesson/correction", tool: "log_lesson",
    re: /\b(lesson|learned|next time|turns out|gotcha|note to self|don'?t forget|the trick is|actually,? it'?s)\b/i },
  { cls: "follow-up", tool: "add_reminder",
    re: /\b(remind me|follow ?up|next session|by (mon|tue|wed|thu|fri|eod|end of)|deadline|\bdue\b|\btodo\b|don'?t forget to)\b/i },
  { cls: "convention/runbook", tool: "remember (kind=runbook or project_convention)",
    re: /\b(from now on|going forward|the procedure is|the steps? (to|are)|convention|the standard is|always do this)\b/i },
];

// Tool names that count as "a save already happened this turn" (suppresses the nudge).
export const SAVE_TOOLS = new Set([
  "remember", "log_decision", "log_lesson", "log_incident", "add_reminder",
  "daimon_remember",
]);

// Match a tool-call name to a save tool, tolerating MCP namespacing (server.tool,
// server__tool, server/tool) since Codex/MCP may prefix the bare tool name.
export function isSaveTool(name) {
  const n = String(name || "").toLowerCase();
  for (const s of SAVE_TOOLS) {
    if (n === s || n.endsWith("." + s) || n.endsWith("__" + s) || n.endsWith("/" + s) || n.endsWith(":" + s)) {
      return true;
    }
  }
  return false;
}

export function scanSignal(text) {
  if (!text) return null;
  for (const s of SIGNALS) if (s.re.test(text)) return s;
  return null;
}

// --- per-session state (hooks are separate processes; the counter must persist to disk) ---
function statePath(sessionId) {
  const safe = String(sessionId || "default").replace(/[^a-zA-Z0-9_-]/g, "_");
  const base = process.env.XDG_STATE_HOME || join(homedir(), ".local", "state");
  try {
    const dir = join(base, "daimon-memory", "nudge");
    mkdirSync(dir, { recursive: true });
    return join(dir, `${safe}.json`);
  } catch {
    return join(tmpdir(), `daimon-nudge-${safe}.json`);
  }
}
export function loadState(sessionId) {
  try { return JSON.parse(readFileSync(statePath(sessionId), "utf8")); }
  catch { return { turn: 0, quietTurns: 0, lastSaveTurn: -1, lastNudgeTurn: -1 }; }
}
export function saveState(sessionId, st) {
  try { writeFileSync(statePath(sessionId), JSON.stringify(st)); } catch { /* best-effort */ }
}

// Given this turn's detected signal + whether a save already happened, advance the state and
// return the nudge text ("" = stay quiet). Mutates `state` in place.
export function decide(state, { signal, didSave }) {
  state.turn = (state.turn || 0) + 1;
  if (didSave) {                       // the agent already saved -> reset, never nag
    state.lastSaveTurn = state.turn;
    state.quietTurns = 0;
    return "";
  }
  if (signal) {                        // a save-worthy moment that was NOT captured
    state.lastNudgeTurn = state.turn;
    return `<daimon-nudge>\nThat looks like a ${signal.cls} that was not captured. If it is durable, save it now with \`${signal.tool}\` (Memory Save Discipline: one event, one record).\n</daimon-nudge>`;
  }
  state.quietTurns = (state.quietTurns || 0) + 1;
  if (CADENCE > 0 && state.quietTurns >= CADENCE) {   // quiet stretch -> capture pass
    state.quietTurns = 0;
    state.lastNudgeTurn = state.turn;
    return `<daimon-nudge>\n${CADENCE} turns since your last save. If anything since is worth keeping (a decision, lesson, incident, or follow-up), call the matching save tool now.\n</daimon-nudge>`;
  }
  return "";
}
