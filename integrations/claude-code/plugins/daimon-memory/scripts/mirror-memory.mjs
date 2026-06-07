#!/usr/bin/env node
// SessionEnd hook: principled auto-capture. Mirrors Claude Code's OWN auto-memory (the
// curated .md files it writes + indexes) into daimon-memory, so memories are shared across
// tools. This reuses Claude's extraction (no daimon-side model, no raw-transcript dumping);
// the server's content-hash dedup makes re-runs idempotent. Best-effort: always exits 0.
import { readStdin, store } from "./lib/daimon.mjs";
import { readFileSync, readdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const input = await readStdin();
const cwd = input.cwd || process.cwd();
// Claude Code encodes the project dir by replacing every non-alphanumeric char with '-'.
const key = cwd.replace(/[^a-zA-Z0-9]/g, "-");
const memdir = join(homedir(), ".claude", "projects", key, "memory");

let files = [];
try {
  files = readdirSync(memdir).filter((f) => f.endsWith(".md") && f !== "MEMORY.md");
} catch {
  process.exit(0); // no memory dir for this project: nothing to mirror
}

for (const f of files) {
  let content = "";
  try { content = readFileSync(join(memdir, f), "utf8"); } catch { continue; }
  if (!content.trim()) continue;

  // Split frontmatter from body; derive title + type from frontmatter when present.
  const fm = content.match(/^---\n([\s\S]*?)\n---\n?/);
  const body = (fm ? content.slice(fm[0].length) : content).trim();
  const name = (content.match(/^name:\s*(.+)$/m) || [])[1];
  const desc = (content.match(/^description:\s*(.+)$/m) || [])[1];
  const type = ((content.match(/^\s*type:\s*(.+)$/m) || [])[1] || "note").trim();
  const title = String(desc || name || f.replace(/\.md$/, "")).trim().slice(0, 120);
  const text = body || title;

  await store({
    kind: "agent_lesson",
    namespace: "claude-private/memory",
    title,
    body: text,
    fields: { lesson: text },
    tags: ["claude-memory", "mirror", type],
    importance: 30,
  });
}
process.exit(0);
