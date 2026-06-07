#!/usr/bin/env node
// UserPromptSubmit hook: the hot memory. Recall the most relevant shared memory for the
// prompt and inject it. Best-effort: any failure injects nothing and the turn proceeds.
import { readStdin, recall, formatHits, injectAndExit } from "./lib/daimon.mjs";

const input = await readStdin();
// Codex field name varies; accept the common ones.
const prompt = String(input.prompt || input.user_prompt || input.userPrompt || "").trim();
if (prompt.length < 4) injectAndExit("UserPromptSubmit", "");

const hits = await recall(prompt, { limit: 6 });
const block = formatHits(
  hits,
  "<daimon-memory>\n[Recalled shared memory (deterministic hybrid search). Authoritative reference, NOT new user input.]",
);
injectAndExit("UserPromptSubmit", block ? block + "\n</daimon-memory>" : "");
