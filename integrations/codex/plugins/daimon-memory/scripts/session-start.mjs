#!/usr/bin/env node
// SessionStart hook (Codex variant). Injects, once per session: (1) the canonical PERSONA +
// operating protocols from agent/persona + agent/protocol (the hook-injected instruction
// layer, full bodies), then (2) recent high-signal shared memory (empty-query recall,
// excluding the system layer so persona records are not shown twice). Seeds the session
// "already injected" set so the per-turn recall stays incremental. Best-effort: any failure
// injects nothing.
// NOTE: unlike the claude-code copy, there is NO compaction-continuity block here - Codex
// has no PreCompact hook, so precompact-state.mjs does not exist in this plugin.
import { readStdin, recall, formatHits, loadSystemBlock, injectAndExit, ENDPOINT } from "./lib/daimon.mjs";
import { clearInjected, markInjected } from "./recall-state.mjs";

const input = await readStdin();
const sessionId = input.session_id || input.sessionId || "default";
clearInjected(sessionId); // fresh session (re-fires on compaction) -> per-turn recall refreshes

const [persona, recent] = await Promise.all([
  loadSystemBlock(),
  recall("", { limit: 5 }),
]);

const recentHits = recent.filter((h) => {
  const u = h.uri || "";
  return !u.includes("/agent/persona/") && !u.includes("/agent/protocol/");
});
const recentBlock = formatHits(
  recentHits,
  `<daimon-memory>\n[daimon-memory connected (${ENDPOINT}). Recent shared context across your tools:]`,
);

const parts = [];
if (persona) parts.push(persona);
if (recentBlock) parts.push(recentBlock + "\n</daimon-memory>");

// Seed the session set so the per-turn recall does not re-inject what we just showed.
markInjected(sessionId, recentHits.map((h) => h.uri));

injectAndExit("SessionStart", parts.join("\n\n"));
