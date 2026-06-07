#!/usr/bin/env bash
# ============================================================================
# daimon-memory -> Claude Code  - interactive CLIENT installer.
#
# Claude Code installs plugins through its own /plugin UI, which this script cannot
# drive. What it CAN do (and /plugin cannot) is set the connection config that the
# plugin's hooks + MCP server read. So this installer:
#   1) writes DAIMON_ENDPOINT / DAIMON_TENANT into Claude Code's settings.json `env`
#   2) prints the two /plugin commands to run inside Claude Code to add + install it
#
# Run with no args for a guided setup:  ./install.sh
# Non-interactive: ./install.sh --endpoint URL --tenant UUID --yes
# ============================================================================
set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETTINGS="${CLAUDE_SETTINGS:-$HOME/.claude/settings.json}"
DEV_TENANT="00000000-0000-0000-0000-0000000000d1"
ENDPOINT=""; TENANT=""; ASSUME_YES=0

while [ $# -gt 0 ]; do
  case "$1" in
    --endpoint) ENDPOINT="$2"; shift 2;;
    --tenant) TENANT="$2"; shift 2;;
    -y|--yes) ASSUME_YES=1; shift;;
    -h|--help) sed -n '2,14p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

hr(){ printf -- '----------------------------------------------------------------\n'; }
bold(){ printf '\033[1m%s\033[0m\n' "$*"; }
is_tty(){ [ -t 0 ] && [ -t 1 ]; }
ask(){ # ask <var> <prompt> <default>  - prompts only if var is empty
  local v="$1" p="$2" d="${3:-}" cur ans=""; eval "cur=\${$v}"; [ -n "$cur" ] && return 0
  if is_tty && [ "$ASSUME_YES" != 1 ]; then read -r -p "  $p [$d]: " ans || true; fi
  printf -v "$v" '%s' "${ans:-$d}"
}

command -v node >/dev/null 2>&1 || echo "WARNING: node not found on PATH; the plugin hooks need it."
[ -f "$SETTINGS" ] || { mkdir -p "$(dirname "$SETTINGS")"; echo '{}' > "$SETTINGS"; }

hr; bold "daimon-memory -> Claude Code"
echo "Configures Claude Code to use daimon-memory (shared, cross-tool memory)."
hr
ask ENDPOINT "daimon-memory endpoint - the URL where your daimon-memory server runs" "http://localhost:8080"
ask TENANT   "Tenant ID - which memory space to use (match your other tools)"        "$DEV_TENANT"

# Best-effort reachability note.
if command -v curl >/dev/null 2>&1; then
  curl -sf --max-time 4 "$ENDPOINT/readyz" >/dev/null 2>&1 \
    && echo "  ok reachable: $ENDPOINT" \
    || echo "  note: could not reach $ENDPOINT/readyz yet (fine; recall is best-effort until it is up)"
fi

echo "Writing DAIMON_ENDPOINT / DAIMON_TENANT into $SETTINGS (env block)..."
cp "$SETTINGS" "$SETTINGS.bak-daimon"
ENDPOINT="$ENDPOINT" TENANT="$TENANT" SETTINGS="$SETTINGS" python3 - <<'PY'
import json, os
p = os.environ["SETTINGS"]
d = json.load(open(p))
env = d.setdefault("env", {})
env["DAIMON_ENDPOINT"] = os.environ["ENDPOINT"]
env["DAIMON_TENANT"] = os.environ["TENANT"]
json.dump(d, open(p, "w"), indent=2); open(p, "a").write("\n")
print("  set env.DAIMON_ENDPOINT =", os.environ["ENDPOINT"])
print("  set env.DAIMON_TENANT   =", os.environ["TENANT"])
PY

hr; bold "Last step: install the plugin in Claude Code"
echo "Run these inside Claude Code (the /plugin UI), then restart:"
echo
echo "  /plugin marketplace add $SELF_DIR"
echo "  /plugin install daimon-memory@daimon-memory"
echo
echo "After restart: relevant memory is auto-recalled into each prompt, and the"
echo "daimon recall/remember/read tools + the /daimon command are available."
echo "Revert config: restore $SETTINGS.bak-daimon"
hr
