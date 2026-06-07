#!/usr/bin/env bash
# ============================================================================
# daimon-memory -> Codex  - automated CLIENT installer.
#
# Codex has a real `codex plugin` CLI, so this fully installs (no in-app step). It stages
# the marketplace into $CODEX_HOME, substitutes the plugin-root + MCP-URL placeholders
# (Codex does not inject a plugin-root env into hook subprocesses), writes the hook config,
# and registers + installs via the CLI.
#
# Usage:  ./install.sh [--endpoint URL] [--tenant UUID] [--yes]
# ============================================================================
set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
DEV_TENANT="00000000-0000-0000-0000-0000000000d1"
ENDPOINT=""; TENANT=""; ASSUME_YES=0

while [ $# -gt 0 ]; do
  case "$1" in
    --endpoint) ENDPOINT="$2"; shift 2;;
    --tenant) TENANT="$2"; shift 2;;
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

# Resolve the real codex binary (bypass any broken shell wrappers).
CODEX="$(command -v codex 2>/dev/null || true)"
[ -x "/Applications/Codex.app/Contents/Resources/codex" ] && CODEX="/Applications/Codex.app/Contents/Resources/codex"
[ -n "$CODEX" ] || { echo "ERROR: codex CLI not found" >&2; exit 1; }
[ -d "$CODEX_HOME" ] || { echo "ERROR: CODEX_HOME not found: $CODEX_HOME" >&2; exit 1; }

hr; bold "daimon-memory -> Codex"
echo "Installs the daimon-memory plugin (hot-memory recall + MCP tools)."
hr
ask ENDPOINT "daimon-memory endpoint - the URL where your daimon-memory server runs" "http://localhost:8080"
ask TENANT   "Tenant ID - which memory space to use (match your other tools)"        "$DEV_TENANT"

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

# Substitute placeholders (Codex doesn't inject CODEX_PLUGIN_ROOT into hooks).
sed -i.bak "s|__DAIMON_PLUGIN_ROOT__|$PLUGIN_DIR|g" "$PLUGIN_DIR/hooks/hooks.json"; rm -f "$PLUGIN_DIR/hooks/hooks.json.bak"
sed -i.bak "s|__DAIMON_MCP_URL__|$ENDPOINT/mcp|g" "$PLUGIN_DIR/.mcp.json"; rm -f "$PLUGIN_DIR/.mcp.json.bak"

# Config the hooks read (Codex hook subprocesses don't get env reliably).
cat > "$PLUGIN_DIR/scripts/lib/daimon.config.json" <<EOF
{ "endpoint": "$ENDPOINT", "tenant": "$TENANT" }
EOF
echo "  baked plugin root, MCP url ($ENDPOINT/mcp), and hook config"

# Register + install via the codex plugin CLI.
echo "registering marketplace + installing plugin..."
"$CODEX" plugin marketplace add "$STABLE" 2>&1 | sed 's/^/  /' || true
"$CODEX" plugin add daimon-memory@daimon-memory 2>&1 | sed 's/^/  /' || true

hr; bold "Done"
echo "Restart Codex. Relevant memory auto-recalls into each prompt; the daimon MCP tools"
echo "(recall/remember/read) are available. Backend: $ENDPOINT"
echo "Uninstall: $CODEX plugin remove daimon-memory ; $CODEX plugin marketplace remove daimon-memory"
hr
