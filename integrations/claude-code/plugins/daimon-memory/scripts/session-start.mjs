#!/usr/bin/env node
// SessionStart hook. Injects, once per session: (1) the canonical PERSONA + operating
// protocols from shared-canonical/system (the hook-injected instruction layer, full bodies),
// then (2) recent high-signal shared memory (empty-query recall, excluding the system layer
// so persona records are not shown twice). Best-effort: any failure injects nothing.
import { readStdin, recall, formatHits, loadSystemBlock, injectAndExit, ENDPOINT } from "./lib/daimon.mjs";

await readStdin(); // consume the payload

const [persona, recent] = await Promise.all([
  loadSystemBlock(),
  recall("", { limit: 5 }),
]);

const recentBlock = formatHits(
  recent.filter((h) => !(h.uri || "").includes("/shared-canonical/system/")),
  `<daimon-memory>\n[daimon-memory connected (${ENDPOINT}). Recent shared context across your tools:]`,
);

const parts = [];
if (persona) parts.push(persona);
if (recentBlock) parts.push(recentBlock + "\n</daimon-memory>");

injectAndExit("SessionStart", parts.join("\n\n"));
