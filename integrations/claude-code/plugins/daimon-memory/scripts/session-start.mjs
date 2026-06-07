#!/usr/bin/env node
// SessionStart hook: seed the session with recent, high-signal shared memory. An empty
// query makes daimon-memory return recent records ranked by importance + recency (the
// "recent context"). Best-effort: failures inject nothing.
import { readStdin, recall, formatHits, injectAndExit, ENDPOINT } from "./lib/daimon.mjs";

await readStdin(); // consume the payload

const hits = await recall("", { limit: 5 });
const block = formatHits(
  hits,
  `<daimon-memory>\n[daimon-memory connected (${ENDPOINT}). Recent shared context across your tools:]`,
);
injectAndExit("SessionStart", block ? block + "\n</daimon-memory>" : "");
