#!/usr/bin/env node
// SessionStart hook: seed recent, high-signal shared memory (empty-query recall ranks by
// importance + recency). Best-effort.
import { readStdin, recall, formatHits, injectAndExit, ENDPOINT } from "./lib/daimon.mjs";

await readStdin();
const hits = await recall("", { limit: 5 });
const block = formatHits(
  hits,
  `<daimon-memory>\n[daimon-memory connected (${ENDPOINT}). Recent shared context across your tools:]`,
);
injectAndExit("SessionStart", block ? block + "\n</daimon-memory>" : "");
