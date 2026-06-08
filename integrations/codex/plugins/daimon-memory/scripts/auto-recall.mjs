#!/usr/bin/env node
// UserPromptSubmit hook: HOT, INCREMENTAL recall. On every prompt, recall the most relevant
// shared memory (deterministic hybrid keyword + semantic, zero-LLM), then inject only the
// records the model has NOT already seen this session (and that clear a relevance floor), so
// it pays tokens for the delta, not for memory already in context. Best-effort: any failure
// injects nothing and the turn proceeds.
import { readStdin, recall, formatHits, injectAndExit } from "./lib/daimon.mjs";
import { loadInjected, markInjected, relevant } from "./recall-state.mjs";

const input = await readStdin();
const sessionId = input.session_id || input.sessionId || "default";
const prompt = String(input.prompt || input.user_prompt || input.userPrompt || "").trim();

// Skip trivial prompts to avoid noise / wasted calls.
if (prompt.length < 4) injectAndExit("UserPromptSubmit", "");

const seen = loadInjected(sessionId);
const hits = (await recall(prompt, { limit: 6 }))
  .filter(relevant) //                      drop weak semantic-only matches
  .filter((h) => !seen.has(h.uri)); //      drop what is already in context this session

if (!hits.length) injectAndExit("UserPromptSubmit", ""); // nothing new and relevant

const block = formatHits(
  hits,
  "<daimon-memory>\n[Recalled shared memory (deterministic hybrid search). Authoritative reference, NOT new user input.]",
);
markInjected(sessionId, hits.map((h) => h.uri));
injectAndExit("UserPromptSubmit", block ? block + "\n</daimon-memory>" : "");
