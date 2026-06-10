#!/usr/bin/env bash
# ============================================================================
# daimon-memory -> Codex  - automated CLIENT installer.
#
# Codex has a real `codex plugin` CLI, so this fully installs (no in-app step). It stages
# the marketplace into $CODEX_HOME, substitutes the plugin-root + MCP-URL placeholders
# (Codex does not inject a plugin-root env into hook subprocesses), writes the hook config,
# and registers + installs via the CLI.
#
# Usage:  ./install.sh [--endpoint URL] [--tenant UUID] [--api-key TOKEN] [--yes]
# ============================================================================
set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
DEV_TENANT="00000000-0000-0000-0000-0000000000d1"
ENDPOINT=""; TENANT=""; API_KEY="${DAIMON_API_KEY:-}"; ASSUME_YES=0

while [ $# -gt 0 ]; do
  case "$1" in
    --endpoint) ENDPOINT="$2"; shift 2;;
    --tenant) TENANT="$2"; shift 2;;
    --api-key) API_KEY="$2"; shift 2;;
    -y|--yes) ASSUME_YES=1; shift;;
    -h|--help) sed -n '2,12p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

bold(){ printf '\033[1m%s\033[0m\n' "$*"; }
hr(){ printf -- '----------------------------------------------------------------\n'; }
is_tty(){ [ -t 0 ] && [ -t 1 ]; }
ask(){ local v="$1" p="$2" d="${3:-}" cur ans=""; eval "cur=\${$v}"; [ -n "$cur" ] && return 0
  if is_tty && [ "$ASSUME_YES" != 1 ]; then read -r -p "  $p [$d]: " ans || true; fi
  printf -v "$v" '%s' "${ans:-$d}"; }

# Portable Python runner: Git Bash on Windows ships no `python3` on PATH, but the `py`
# launcher is standard there. An alias would not expand in a non-interactive script, so
# wrap the dispatch in a function instead.
have_python(){ command -v python3 >/dev/null 2>&1 || command -v py >/dev/null 2>&1; }
pyrun(){
  if command -v python3 >/dev/null 2>&1; then python3 "$@"
  elif command -v py >/dev/null 2>&1; then py -3 "$@"
  else return 127; fi
}

# Idempotently enable Codex native memory ([features] memories = true) without clobbering the
# existing [features] table. Handles all four states (already-on / present-but-false / table
# exists / table missing). Uses Python for a safe targeted edit; no-op if Python is absent.
enable_native_memory(){
  local cfg="$CODEX_HOME/config.toml"
  have_python || { echo "  (python3 not found; enable manually: Codex Settings -> Memory)"; return 1; }
  pyrun - "$cfg" <<'PY'
import sys, os, re
p = sys.argv[1]
s = open(p).read() if os.path.exists(p) else ""
if re.search(r'(?m)^\s*memories\s*=\s*true', s):
    print("  native memory already enabled"); sys.exit(0)
if re.search(r'(?m)^\s*#?\s*memories\s*=', s):
    s = re.sub(r'(?m)^\s*#?\s*memories\s*=.*$', 'memories = true', s, count=1)
    open(p, 'w').write(s); print("  flipped existing memories key -> true"); sys.exit(0)
m = re.search(r'(?m)^\[features\]\s*$', s)
if m:
    i = m.end(); s = s[:i] + "\nmemories = true" + s[i:]
    open(p, 'w').write(s); print("  added memories = true under existing [features]"); sys.exit(0)
s = s + ("\n" if s and not s.endswith("\n") else "") + "\n[features]\nmemories = true\n"
open(p, 'w').write(s); print("  appended [features] memories = true")
PY
}

# Resolve the real codex binary (bypass any broken shell wrappers).
CODEX="$(command -v codex 2>/dev/null || true)"
[ -x "/Applications/Codex.app/Contents/Resources/codex" ] && CODEX="/Applications/Codex.app/Contents/Resources/codex"
[ -n "$CODEX" ] || { echo "ERROR: codex CLI not found" >&2; exit 1; }
[ -d "$CODEX_HOME" ] || { echo "ERROR: CODEX_HOME not found: $CODEX_HOME" >&2; exit 1; }
node -e 'import("node:sqlite").then(()=>process.exit(0)).catch(()=>process.exit(1))' 2>/dev/null \
  || echo "note: node:sqlite not found in PATH node; the native-memory mirror is skipped until node >=22.5 (recall/persona/nudge unaffected)."

hr; bold "daimon-memory -> Codex"
echo "Installs the daimon-memory plugin (hot-memory recall + MCP tools)."
hr
ask ENDPOINT "daimon-memory endpoint - the URL where your daimon-memory server runs" "http://localhost:8080"
ask TENANT   "Tenant ID - which memory space to use (match your other tools)"        "$DEV_TENANT"
ask API_KEY  "API key - bearer token if the server sets DAIMON_API_KEY (empty = no auth)" ""
[ "$API_KEY" = "none" ] && API_KEY=""

if command -v curl >/dev/null 2>&1; then
  curl -sf --max-time 4 "$ENDPOINT/readyz" >/dev/null 2>&1 \
    && echo "  ok reachable: $ENDPOINT" || echo "  note: $ENDPOINT/readyz not reachable yet (recall is best-effort until it is up)"
fi

# Stage the marketplace into a stable location and bake in absolute paths.
STABLE="$CODEX_HOME/daimon-memory-marketplace"
echo "staging marketplace -> $STABLE"
rm -rf "$STABLE"; mkdir -p "$STABLE"
cp -R "$SELF_DIR/.claude-plugin" "$SELF_DIR/plugins" "$STABLE/"
PLUGIN_DIR="$STABLE/plugins/daimon-memory"

# Substitute placeholders (Codex doesn't inject CODEX_PLUGIN_ROOT into hooks). Python is
# preferred: MSYS sed (Git Bash for Windows) handles -i.bak differently from GNU/BSD sed
# and can mangle Windows paths; sed remains the fallback for minimal POSIX hosts.
if have_python; then
  pyrun - "$PLUGIN_DIR" "$ENDPOINT" "$API_KEY" <<'PY'
import sys, pathlib
plugin_dir, endpoint, api_key = sys.argv[1], sys.argv[2], sys.argv[3]
substitutions = [
    ("hooks/hooks.json", [("__DAIMON_PLUGIN_ROOT__", plugin_dir)]),
    (".mcp.json",        [("__DAIMON_MCP_URL__", endpoint + "/mcp"),
                          ("__DAIMON_API_KEY__", api_key)]),
]
for rel, pairs in substitutions:
    p = pathlib.Path(plugin_dir) / rel
    text = p.read_text(encoding="utf-8")
    for placeholder, value in pairs:
        text = text.replace(placeholder, value)
    p.write_text(text, encoding="utf-8")
PY
else
  sed -i.bak "s|__DAIMON_PLUGIN_ROOT__|$PLUGIN_DIR|g" "$PLUGIN_DIR/hooks/hooks.json"; rm -f "$PLUGIN_DIR/hooks/hooks.json.bak"
  sed -i.bak "s|__DAIMON_MCP_URL__|$ENDPOINT/mcp|g" "$PLUGIN_DIR/.mcp.json"; rm -f "$PLUGIN_DIR/.mcp.json.bak"
  sed -i.bak "s|__DAIMON_API_KEY__|$API_KEY|g" "$PLUGIN_DIR/.mcp.json"; rm -f "$PLUGIN_DIR/.mcp.json.bak"
fi

# Config the hooks read (Codex hook subprocesses don't get env reliably). lib/daimon.mjs
# loads this file as the fallback between env and the dev defaults. Prefer python3 so a key
# containing JSON-special characters can't silently produce a malformed (= ignored) config.
if have_python; then
  ENDPOINT="$ENDPOINT" TENANT="$TENANT" API_KEY="$API_KEY" CFG="$PLUGIN_DIR/scripts/lib/daimon.config.json" pyrun - <<'PY'
import json, os
json.dump({"endpoint": os.environ["ENDPOINT"], "tenant": os.environ["TENANT"],
           "apiKey": os.environ["API_KEY"]}, open(os.environ["CFG"], "w"))
PY
else
  cat > "$PLUGIN_DIR/scripts/lib/daimon.config.json" <<EOF
{ "endpoint": "$ENDPOINT", "tenant": "$TENANT", "apiKey": "$API_KEY" }
EOF
fi
chmod 600 "$PLUGIN_DIR/scripts/lib/daimon.config.json"
echo "  baked plugin root, MCP url ($ENDPOINT/mcp), and hook config"

# Enable Codex native memory so the mirror has something to read (best-effort, idempotent;
# defaults handle idle/age/secret-redaction). Manual fallback: Codex Settings -> Memory.
echo "enabling Codex native memory in $CODEX_HOME/config.toml"
enable_native_memory || echo "  enable manually: Codex Settings -> Memory, then restart"

# Register + install via the codex plugin CLI.
echo "registering marketplace + installing plugin..."
"$CODEX" plugin marketplace add "$STABLE" 2>&1 | sed 's/^/  /' || true
"$CODEX" plugin add daimon-memory@daimon-memory 2>&1 | sed 's/^/  /' || true

hr; bold "Done"
echo "Restart Codex. Relevant memory auto-recalls into each prompt; the daimon MCP tools"
echo "(recall/remember/read) are available. Backend: $ENDPOINT"
echo "Codex native memory enabled; prior-session memories mirror into daimon (agent/lessons)"
echo "on the next session start (Codex writes native memory only after a thread idles ~6h)."
echo "Uninstall: $CODEX plugin remove daimon-memory ; $CODEX plugin marketplace remove daimon-memory"
hr
