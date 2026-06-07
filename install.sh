#!/usr/bin/env bash
# ============================================================================
# daimon-memory — interactive installer for the MEMORY BACKEND (server).
#
# Run with no args for a guided setup:   ./install.sh
# It asks how you want to run daimon-memory, prompts for each config value (with
# a plain-language explanation + a sensible default), writes .env, and (for the
# Docker path) brings the stack up.
#
# Non-interactive (CI / automation): pass --yes to accept all defaults, or set
# the env vars below before running. To connect a tool (Hermes/Claude/Codex)
# AFTER the server is up, use the client installer: integrations/<tool>/install.sh
# ============================================================================
set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$SELF_DIR/.env"
DEV_TENANT="00000000-0000-0000-0000-0000000000d1"

ASSUME_YES=0
[ "${1:-}" = "--yes" ] || [ "${1:-}" = "-y" ] && ASSUME_YES=1

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
dim()  { printf '\033[2m%s\033[0m\n' "$*"; }
hr()   { printf -- '----------------------------------------------------------------\n'; }
is_tty() { [ -t 0 ] && [ -t 1 ]; }

# ask <var> <prompt> <default>   — prompts (interactive) or uses default
ask() {
  local __var="$1" __prompt="$2" __def="${3:-}" __ans=""
  if is_tty && [ "$ASSUME_YES" != 1 ]; then
    if [ -n "$__def" ]; then read -r -p "  $__prompt [$__def]: " __ans || true
    else read -r -p "  $__prompt: " __ans || true; fi
  fi
  printf -v "$__var" '%s' "${__ans:-$__def}"
}
# ask_secret <var> <prompt>  — hidden input
ask_secret() {
  local __var="$1" __prompt="$2" __ans=""
  if is_tty && [ "$ASSUME_YES" != 1 ]; then
    read -r -s -p "  $__prompt: " __ans || true; printf '\n'
  fi
  printf -v "$__var" '%s' "$__ans"
}
confirm() {  # confirm <prompt>  (default No)
  if ! is_tty || [ "$ASSUME_YES" = 1 ]; then return 0; fi
  local a=""; read -r -p "  $1 [y/N]: " a || true
  case "$a" in y|Y|yes|YES) return 0;; *) return 1;; esac
}
gen_uuid() {
  uuidgen 2>/dev/null | tr 'A-Z' 'a-z' \
    || python3 -c 'import uuid;print(uuid.uuid4())' 2>/dev/null \
    || echo "$DEV_TENANT"
}
backup_env() { [ -f "$ENV_FILE" ] && cp "$ENV_FILE" "$ENV_FILE.bak.$(date +%s 2>/dev/null || echo bak)" && dim "  (backed up existing .env)"; return 0; }

hr; bold "daimon-memory installer"
echo "Sets up the shared memory backend: PostgreSQL (truth) + Qdrant (search) + the API."
hr

echo "How do you want to run daimon-memory?"
echo "  1) Docker Compose  — bundles PostgreSQL + Qdrant + the server (easiest)"
echo "  2) My own PostgreSQL + Qdrant — write config; you run the binaries"
ask RUN_MODE "Choose 1 or 2" "1"
echo

if [ "$RUN_MODE" = "2" ]; then
  bold "Connect to your existing PostgreSQL + Qdrant"
  echo "Where daimon-memory should store and search your memories."
  ask    PGHOST     "PostgreSQL host"                         "localhost"
  ask    PGPORT     "PostgreSQL port"                         "5432"
  ask    PGUSER     "PostgreSQL user"                         "daimon"
  ask_secret PGPASSWORD "PostgreSQL password (input hidden)"
  ask    PGDATABASE "PostgreSQL database name"                "daimon_memory"
  ask    QURL       "Qdrant gRPC URL (note: gRPC port 6334, not the 6333 REST port)" "http://localhost:6334"
  ask    BINDADDR   "API listen address (host:port)"          "0.0.0.0:8080"
  ask    TENANT     "Default tenant ID (groups your memories; keep default unless you run isolated spaces)" "$DEV_TENANT"
  echo
  bold "Summary"
  echo "  Postgres : ${PGUSER}@${PGHOST}:${PGPORT}/${PGDATABASE}"
  echo "  Qdrant   : ${QURL}"
  echo "  API bind : ${BINDADDR}"
  echo "  Tenant   : ${TENANT}"
  echo
  confirm "Write this to $ENV_FILE ?" || { echo "Aborted."; exit 0; }
  backup_env
  cat > "$ENV_FILE" <<EOF
# daimon-memory runtime config (source this before running the binaries).
PGHOST=$PGHOST
PGPORT=$PGPORT
PGUSER=$PGUSER
PGPASSWORD=$PGPASSWORD
PGDATABASE=$PGDATABASE
DAIMON_QDRANT_URL=$QURL
DAIMON_MCP_BIND=$BINDADDR
DAIMON_DEFAULT_TENANT=$TENANT
RUST_LOG=info
EOF
  chmod 600 "$ENV_FILE"
  echo "Wrote $ENV_FILE"
  hr; bold "Next steps"
  echo "  set -a; source .env; set +a"
  echo "  daimon migrate          # apply the schema"
  echo "  daimon-mcp              # start the API (serves \$DAIMON_MCP_BIND)"
  echo "  daimon-indexer          # start the outbox->Qdrant indexer (separate process)"
  echo "(build the binaries first with: cargo build --release)"
  hr
  exit 0
fi

# ---- Docker Compose path ----
bold "Run with Docker Compose"
if ! command -v docker >/dev/null 2>&1; then
  echo "Docker not found on PATH. Install Docker Desktop / Engine, or re-run and choose option 2."
  exit 1
fi
echo "daimon-memory will run alongside a bundled PostgreSQL + Qdrant."
ask API_PORT  "API port — where the memory API listens on your machine" "8080"
ask PG_PASS   "PostgreSQL password for the bundled DB (blank = auto-generate)" ""
[ -z "$PG_PASS" ] && PG_PASS="$(gen_uuid | tr -d '-' | cut -c1-24)" && dim "  (generated a password)"
ask TENANT    "Default tenant ID (keep default unless you run isolated memory spaces)" "$DEV_TENANT"
echo
bold "Summary"
echo "  API URL  : http://localhost:${API_PORT}"
echo "  Tenant   : ${TENANT}"
echo "  Postgres : bundled (password saved to .env)"
echo
confirm "Write $ENV_FILE and start the stack now?" || { echo "Aborted (nothing started)."; exit 0; }
backup_env
cat > "$ENV_FILE" <<EOF
# daimon-memory — docker compose config (consumed by docker-compose.yml).
DAIMON_PORT=$API_PORT
DAIMON_PG_PASSWORD=$PG_PASS
DAIMON_DEFAULT_TENANT=$TENANT
EOF
chmod 600 "$ENV_FILE"
echo "Wrote $ENV_FILE"
echo
echo "Starting (first run builds the image + downloads the embedding model; this is slow once)..."
( cd "$SELF_DIR" && docker compose up -d --build )
echo
hr; bold "daimon-memory is starting"
echo "  API:        http://localhost:${API_PORT}/readyz"
echo "  Logs:       docker compose logs -f daimon-mcp"
echo "  Stop:       docker compose down        (add -v to also delete data)"
echo
echo "Next: connect a tool with the client installer, e.g."
echo "  integrations/hermes/install.sh --endpoint http://localhost:${API_PORT}"
hr
