// Unit tests for the deterministic save-nudge engine (node --test, zero deps).
//   node --test integrations/tests/
// Lives OUTSIDE plugins/.../scripts/ so the CI hooks-smoke glob (which executes each
// script with a stub stdin) does not pick it up.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const CC = "../claude-code/plugins/daimon-memory/scripts/";
const CX = "../codex/plugins/daimon-memory/scripts/";
const lib = await import(CC + "nudge-lib.mjs");
const { scanSignal, isSaveTool, decide } = lib;

const p = (rel) => fileURLToPath(new URL(rel, import.meta.url));
const read = (rel) => readFileSync(p(rel), "utf8");

test("scanSignal maps each save-signal class to its tool", () => {
  assert.equal(scanSignal("we decided to use Postgres").tool, "log_decision");
  assert.equal(scanSignal("that broke the build, root cause was X").tool, "log_incident");
  assert.equal(scanSignal("lesson learned: always pin the image").tool, "log_lesson");
  assert.equal(scanSignal("remind me to reindex tomorrow").tool, "add_reminder");
  assert.match(scanSignal("from now on we tag releases").tool, /runbook|project_convention/);
});

test("scanSignal returns null on prose with no signal", () => {
  assert.equal(scanSignal("the function returns a list of users"), null);
  assert.equal(scanSignal(""), null);
  assert.equal(scanSignal(null), null);
});

test("isSaveTool tolerates MCP namespacing", () => {
  assert.ok(isSaveTool("log_decision"));
  assert.ok(isSaveTool("daimon_remember"));
  assert.ok(isSaveTool("mcp__daimon__log_decision")); // server__tool
  assert.ok(isSaveTool("daimon.remember")); // server.tool
  assert.ok(isSaveTool("daimon/add_reminder")); // server/tool
  assert.equal(isSaveTool("recall"), false);
  assert.equal(isSaveTool("read"), false);
  assert.equal(isSaveTool(""), false);
});

test("decide: a save this turn suppresses the nudge and resets quiet counter", () => {
  const st = { turn: 4, quietTurns: 3, lastSaveTurn: -1, lastNudgeTurn: -1 };
  const out = decide(st, { signal: scanSignal("we decided X"), didSave: true });
  assert.equal(out, "");
  assert.equal(st.quietTurns, 0);
  assert.equal(st.lastSaveTurn, 5);
});

test("decide: an uncaptured signal nudges and names the tool", () => {
  const st = { turn: 0, quietTurns: 0, lastSaveTurn: -1, lastNudgeTurn: -1 };
  const out = decide(st, { signal: scanSignal("we chose Qdrant"), didSave: false });
  assert.match(out, /log_decision/);
  assert.match(out, /daimon-nudge/);
});

test("decide: cadence nudge fires at exactly N quiet turns then resets", () => {
  // Default cadence is 5 (no DAIMON_NUDGE_CADENCE in the test env).
  const st = { turn: 0, quietTurns: 0, lastSaveTurn: -1, lastNudgeTurn: -1 };
  const quiet = { signal: null, didSave: false };
  for (let i = 0; i < 4; i++) assert.equal(decide(st, quiet), "", `turn ${i + 1} should stay quiet`);
  const fifth = decide(st, quiet);
  assert.match(fifth, /turns since your last save/);
  assert.equal(st.quietTurns, 0, "counter resets after firing");
});

// --- state path resolution (issue #5/#8: %LOCALAPPDATA% on Windows, XDG elsewhere) ---
const { stateBase } = await import(CC + "lib/state-paths.mjs");

test("stateBase: XDG_STATE_HOME wins on every platform", () => {
  const prev = process.env.XDG_STATE_HOME;
  process.env.XDG_STATE_HOME = "/tmp/xdg-test";
  try { assert.equal(stateBase(), "/tmp/xdg-test"); }
  finally { prev === undefined ? delete process.env.XDG_STATE_HOME : process.env.XDG_STATE_HOME = prev; }
});

test("stateBase: platform fallback is LOCALAPPDATA on win32, ~/.local/state elsewhere", () => {
  const prev = process.env.XDG_STATE_HOME;
  delete process.env.XDG_STATE_HOME;
  try {
    const base = stateBase();
    if (process.platform === "win32") {
      assert.ok(/AppData[\\/]Local|%?LOCALAPPDATA/i.test(base) || base === process.env.LOCALAPPDATA,
        `expected a LOCALAPPDATA-style path, got ${base}`);
    } else {
      assert.match(base, /\.local[\\/]state$/);
    }
  } finally { if (prev !== undefined) process.env.XDG_STATE_HOME = prev; }
});

// --- cross-copy parity: the genuinely shared scripts MUST stay byte-identical between
// plugins. (session-start.mjs and mirror-memory.mjs are intentionally per-client and are
// NOT listed here - Codex has no PreCompact hook and reads native memory from SQLite.)
const SHARED = ["nudge-lib.mjs", "nudge.mjs", "auto-recall.mjs", "recall-state.mjs",
  "lib/daimon.mjs", "lib/state-paths.mjs"];
for (const f of SHARED) {
  test(`parity: ${f} identical across claude-code and codex`, () => {
    assert.equal(read(CC + f), read(CX + f), `${f} drifted between the two plugin copies`);
  });
}
