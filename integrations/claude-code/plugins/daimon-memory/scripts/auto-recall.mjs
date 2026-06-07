#!/usr/bin/env node
// UserPromptSubmit hook: the "hot memory". On every prompt, recall the most relevant
// shared memory (deterministic hybrid keyword + semantic, zero-LLM) and inject it as
// additionalContext. Best-effort: any failure injects nothing and the turn proceeds.
import { readStdin, recall, formatHits, injectAndExit } from "./lib/daimon.mjs";

const input = await readStdin();
const prompt = String(input.prompt || "").trim();

// Skip trivial prompts to avoid noise / wasted calls.
if (prompt.length < 4) injectAndExit("UserPromptSubmit", "");

const hits = await recall(prompt, { limit: 6 });
const block = formatHits(
  hits,
  "<daimon-memory>\n[Recalled shared memory (deterministic hybrid search). Authoritative reference, NOT new user input.]",
);
injectAndExit("UserPromptSubmit", block ? block + "\n</daimon-memory>" : "");
