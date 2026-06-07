#!/usr/bin/env bash
# ============================================================================
# daimon-memory -> Hermes  - interactive CLIENT installer.
#
# Connects Hermes to your daimon-memory backend by installing the `daimon` memory
# provider into $HERMES_HOME/plugins/daimon/ and saving its config to $HERMES_HOME/.env.
#
# Run with no args for a guided setup (prompts for each value with an explanation):
#   ./install.sh
# Non-interactive (CI / automation) - pass any value as a flag, add --yes to skip prompts:
#   ./install.sh --endpoint http://localhost:8080 --activate --yes
#   ./install.sh --uninstall
#
# Safety: append-only .env edits (backed up); config.yaml backed up before activation.
# DAIMON_ENDPOINT/TENANT/NAMESPACE are not secrets. Activation is opt-in (only one
# external memory provider runs at a time, so it switches off your current one).
# ============================================================================
set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
DEV_TENANT="00000000-0000-0000-0000-0000000000d1"

ENDPOINT=""; TENANT=""; NAMESPACE=""        # empty => prompt (or default)
ACTIVATE=""; UNINSTALL=0; ASSUME_YES=0

while [ $# -gt 0 ]; do
  case "$1" in
    --endpoint) ENDPOINT="$2"; shift 2;;
    --tenant) TENANT="$2"; shift 2;;
    --namespace) NAMESPACE="$2"; shift 2;;
    --activate) ACTIVATE=1; shift;;
    --no-activate) ACTIVATE=0; shift;;
    --uninstall) UNINSTALL=1; shift;;
    -y|--yes) ASSUME_YES=1; shift;;
    -h|--help) sed -n '2,21p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

PLUGIN_SRC="$SELF_DIR/daimon"
PLUGIN_DEST="$HERMES_HOME/plugins/daimon"
ENV_FILE="$HERMES_HOME/.env"
CONFIG="$HERMES_HOME/config.yaml"

hr()  { printf -- '----------------------------------------------------------------\n'; }
bold(){ printf '\033[1m%s\033[0m\n' "$*"; }
is_tty() { [ -t 0 ] && [ -t 1 ]; }
ask() { # ask <var> <prompt> <default>  - only prompts if the var is still empty
  local __var="$1" __prompt="$2" __def="${3:-}" __cur __ans=""
  eval "__cur=\${$__var}"
  [ -n "$__cur" ] && return 0
  if is_tty && [ "$ASSUME_YES" != 1 ]; then
    read -r -p "  $__prompt [$__def]: " __ans || true
  fi
  printf -v "$__var" '%s' "${__ans:-$__def}"
}
confirm() { # default No
  if ! is_tty || [ "$ASSUME_YES" = 1 ]; then return 1; fi
  local a=""; read -r -p "  $1 [y/N]: " a || true
  case "$a" in y|Y|yes|YES) return 0;; *) return 1;; esac
}

command -v hermes >/dev/null 2>&1 || { echo "ERROR: 'hermes' not on PATH" >&2; exit 1; }
[ -d "$HERMES_HOME" ] || { echo "ERROR: HERMES_HOME not found: $HERMES_HOME" >&2; exit 1; }

if [ "$UNINSTALL" = 1 ]; then
  rm -rf "$PLUGIN_DEST"
  echo "removed: $PLUGIN_DEST"
  echo "Note: env vars + memory.provider were left untouched."
  echo "  Revert the active provider with: hermes memory setup openviking   (or: hermes memory off)"
  exit 0
fi

hr; bold "daimon-memory -> Hermes"
echo "Connects Hermes to your daimon-memory backend (shared, cross-tool memory)."
hr

ask ENDPOINT  "daimon-memory endpoint - the URL where your daimon-memory server is running" "http://localhost:8080"
ask TENANT    "Tenant ID - which memory space Hermes uses (must match your other tools)"    "$DEV_TENANT"
ask NAMESPACE "Default namespace - where Hermes files the memories it captures"             "hermes-private/notes"

# Best-effort reachability check (informational only).
if command -v curl >/dev/null 2>&1; then
  if curl -sf --max-time 4 "$ENDPOINT/readyz" >/dev/null 2>&1; then
    echo "  ✓ reachable: $ENDPOINT"
  else
    echo "  ! could not reach $ENDPOINT/readyz yet (that's fine - recall/capture are best-effort until it's up)"
  fi
fi

# Decide activation: explicit flag wins; otherwise ask.
if [ -z "$ACTIVATE" ]; then
  echo
  echo "Activating daimon makes it Hermes's ACTIVE memory provider and turns off your"
  echo "current one (only one external provider runs at a time)."
  if confirm "Make daimon the active provider now?"; then ACTIVATE=1; else ACTIVATE=0; fi
fi

echo
bold "Summary"
echo "  Endpoint : $ENDPOINT"
echo "  Tenant   : $TENANT"
echo "  Namespace: $NAMESPACE"
echo "  Activate : $([ "$ACTIVATE" = 1 ] && echo yes || echo "no (install only)")"
echo
if is_tty && [ "$ASSUME_YES" != 1 ]; then
  confirm "Proceed?" || { echo "Aborted."; exit 0; }
fi

# 1) install the plugin
mkdir -p "$PLUGIN_DEST"
cp "$PLUGIN_SRC/__init__.py" "$PLUGIN_DEST/"
cp "$PLUGIN_SRC/plugin.yaml" "$PLUGIN_DEST/"
echo "installed plugin  -> $PLUGIN_DEST"

# 2) register config in .env (backup once, append-only, idempotent)
touch "$ENV_FILE"; chmod 600 "$ENV_FILE"
[ -f "$ENV_FILE.bak-daimon" ] || cp "$ENV_FILE" "$ENV_FILE.bak-daimon"
add_env() {
  local k="$1" v="$2"
  if grep -q "^${k}=" "$ENV_FILE" 2>/dev/null; then
    echo "  = ${k} (already set; edit $ENV_FILE to change)"
  else
    printf '%s=%s\n' "$k" "$v" >> "$ENV_FILE"; echo "  + ${k}"
  fi
}
echo "configuring $ENV_FILE (backup: $ENV_FILE.bak-daimon):"
add_env DAIMON_ENDPOINT  "$ENDPOINT"
add_env DAIMON_TENANT    "$TENANT"
add_env DAIMON_NAMESPACE "$NAMESPACE"

# 3) activate (opt-in)
if [ "$ACTIVATE" = 1 ]; then
  [ -f "$CONFIG.bak-daimon" ] || { [ -f "$CONFIG" ] && cp "$CONFIG" "$CONFIG.bak-daimon"; }
  if hermes config set memory.provider daimon >/dev/null 2>&1; then
    echo "ACTIVATED: memory.provider=daimon (config backup: $CONFIG.bak-daimon)"
  else
    echo "WARN: 'hermes config set' failed - activate manually:  hermes memory setup daimon"
  fi
  echo "Restart your Hermes session for the change to take effect."
else
  echo
  echo "Installed (not activated). Activate later with:  hermes config set memory.provider daimon"
fi

echo
echo "Status:"
hermes memory status 2>&1 | head -8 || true
