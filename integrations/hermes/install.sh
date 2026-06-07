#!/usr/bin/env bash
# daimon-memory — installer for Hermes Agent.
#
# Installs the `daimon` memory provider into $HERMES_HOME/plugins/daimon/ and
# registers its (non-secret) config in $HERMES_HOME/.env. Idempotent + reversible.
# Activation (switching Hermes's active memory provider to daimon) is OPT-IN via
# --activate, because only one external provider runs at a time and you may have
# another (e.g. openviking) active.
#
# Usage:
#   ./install.sh [--endpoint URL] [--tenant UUID] [--namespace NS] [--activate]
#   ./install.sh --uninstall
#
# Safety: never rewrites existing .env lines (append-only, backed up); backs up
# config.yaml before activation; DAIMON_ENDPOINT/TENANT/NAMESPACE are NOT secrets.
set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"

ENDPOINT="http://10.100.30.27"
TENANT="00000000-0000-0000-0000-0000000000d1"
NAMESPACE="hermes-private/notes"
ACTIVATE=0
UNINSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --endpoint) ENDPOINT="$2"; shift 2;;
    --tenant) TENANT="$2"; shift 2;;
    --namespace) NAMESPACE="$2"; shift 2;;
    --activate) ACTIVATE=1; shift;;
    --uninstall) UNINSTALL=1; shift;;
    -h|--help) sed -n '2,18p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

PLUGIN_SRC="$SELF_DIR/daimon"
PLUGIN_DEST="$HERMES_HOME/plugins/daimon"
ENV_FILE="$HERMES_HOME/.env"
CONFIG="$HERMES_HOME/config.yaml"

command -v hermes >/dev/null 2>&1 || { echo "ERROR: 'hermes' not on PATH" >&2; exit 1; }
[ -d "$HERMES_HOME" ] || { echo "ERROR: HERMES_HOME not found: $HERMES_HOME" >&2; exit 1; }

if [ "$UNINSTALL" = 1 ]; then
  rm -rf "$PLUGIN_DEST"
  echo "removed: $PLUGIN_DEST"
  echo "Note: env vars + memory.provider were left untouched."
  echo "  To revert the active provider: hermes memory setup openviking   (or: hermes memory off)"
  exit 0
fi

# 1) install the plugin
mkdir -p "$PLUGIN_DEST"
cp "$PLUGIN_SRC/__init__.py" "$PLUGIN_DEST/"
cp "$PLUGIN_SRC/plugin.yaml" "$PLUGIN_DEST/"
echo "installed plugin  -> $PLUGIN_DEST"

# 2) register non-secret config in .env (backup once, append-only, idempotent)
touch "$ENV_FILE"; chmod 600 "$ENV_FILE"
[ -f "$ENV_FILE.bak-daimon" ] || cp "$ENV_FILE" "$ENV_FILE.bak-daimon"
add_env() {
  local k="$1" v="$2"
  if grep -q "^${k}=" "$ENV_FILE" 2>/dev/null; then
    echo "  = ${k} (exists, unchanged)"
  else
    printf '%s=%s\n' "$k" "$v" >> "$ENV_FILE"
    echo "  + ${k}=${v}"
  fi
}
echo "configuring $ENV_FILE (backup: $ENV_FILE.bak-daimon):"
add_env DAIMON_ENDPOINT  "$ENDPOINT"
add_env DAIMON_TENANT    "$TENANT"
add_env DAIMON_NAMESPACE "$NAMESPACE"

# 3) verify discovery
echo "=== hermes memory (discovery) ==="
hermes memory status 2>&1 | head -8 || true

# 4) activation (opt-in)
if [ "$ACTIVATE" = 1 ]; then
  [ -f "$CONFIG.bak-daimon" ] || { [ -f "$CONFIG" ] && cp "$CONFIG" "$CONFIG.bak-daimon"; }
  if hermes config set memory.provider daimon >/dev/null 2>&1; then
    echo "ACTIVATED: memory.provider=daimon (config backup: $CONFIG.bak-daimon)"
  else
    echo "WARN: 'hermes config set' failed — activate manually:"
    echo "  hermes memory setup daimon   (or set 'memory.provider: daimon' in $CONFIG)"
  fi
  echo "Restart your Hermes session for the change to take effect."
else
  echo
  echo "Plugin installed but NOT activated (your current provider is unchanged)."
  echo "To activate daimon as the memory provider (switches off the current one):"
  echo "  hermes config set memory.provider daimon     # then restart hermes"
  echo "Backend must be reachable at: $ENDPOINT  (set per --endpoint / DAIMON_ENDPOINT)"
fi
