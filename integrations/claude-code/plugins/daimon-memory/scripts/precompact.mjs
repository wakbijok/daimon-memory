#!/usr/bin/env node
// PreCompact hook (Claude Code). Compaction-only continuity: just before the context is
// compacted, snapshot the volatile working state the summary tends to lose (git branch, cwd,
// dirty files, active plan) to a LOCAL plugin-owned file. session-start.mjs re-injects it
// after compaction (SessionStart source=compact). Local-only by design: ephemeral scratch,
// never the daimon backend. Best-effort: always exits 0; injects nothing.
import { readStdin } from "./lib/daimon.mjs";
import { savePrecompact } from "./precompact-state.mjs";
import { execSync } from "node:child_process";

const input = await readStdin();
const sessionId = input.session_id || input.sessionId || "default";

function sh(cmd) {
  try {
    return execSync(cmd, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return "";
  }
}

const branch = sh("git rev-parse --abbrev-ref HEAD") || "(not a git repo)";
const dirty = sh("git status --short").split("\n").filter(Boolean).slice(0, 12);
const plan = sh("ls -t docs/plans/*.md docs/superpowers/plans/*.md .claude/plans/*.md 2>/dev/null | head -1");

savePrecompact(sessionId, {
  ts: new Date().toISOString(),
  trigger: input.trigger || "auto",
  branch,
  cwd: process.cwd(),
  dirty,
  plan: plan || null,
});

process.exit(0);
