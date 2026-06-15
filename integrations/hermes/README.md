# daimon-hermes

The **daimon-memory** memory provider for the [NousResearch Hermes Agent](https://github.com/nousresearch/hermes-agent) —
shared, cross-tool context: deterministic hybrid recall (keyword + semantic, zero-LLM)
plus curated, typed capture (no raw-turn dumping).

## Install

```bash
pip install daimon-hermes
daimon-hermes setup            # guided: prompts for endpoint / tenant / namespace / api key
```

Non-interactive (CI / automation), pointing at a **remote** daimon-memory server:

```bash
daimon-hermes setup \
  --endpoint https://daimon.example.com:8080 \
  --tenant   00000000-0000-0000-0000-0000000000d1 \
  --namespace agent/lessons \
  --api-key  "$DAIMON_API_KEY" \
  --activate --yes
```

`setup` does three things (all idempotent; `~/.hermes/.env` is backed up once):

1. writes a 2-line shim into `~/.hermes/plugins/daimon/` that re-exports this package
   (so `pip install -U daimon-hermes` upgrades the provider with **no re-setup**);
2. records `DAIMON_ENDPOINT` / `DAIMON_TENANT` / `DAIMON_NAMESPACE` / `DAIMON_API_KEY`
   in `~/.hermes/.env`;
3. activates it (`hermes config set memory.provider daimon`) unless `--no-activate`.

Restart your Hermes session for the change to take effect.

## Configuration

Configurable at install time (flags above) **or** at runtime via environment — the
provider reads these every session, so pointing at a different server is just an env change:

| Env var | Required | Default | Meaning |
|---|---|---|---|
| `DAIMON_ENDPOINT` | yes | — (provider inert if unset; installer *prompts* `http://localhost:8080`) | daimon-memory base URL (local or remote) |
| `DAIMON_TENANT` | no | dev tenant | memory space (must match your other tools) |
| `DAIMON_NAMESPACE` | no | `agent/lessons` | default capture namespace |
| `DAIMON_API_KEY` | no | — | bearer token (if daimon-mcp auth is enabled) |
| `DAIMON_NUDGE` / `DAIMON_NUDGE_CADENCE` | no | `on` / `5` | save-nudge toggle + cadence |

## Uninstall

```bash
daimon-hermes uninstall        # removes the plugin shim; leaves your .env + provider choice
```

## Develop / test

```bash
cd integrations/hermes           # tests resolve the package relative to this dir
python3 tests/test_provider.py   # provider behaviour (recall warming, on_turn_start)
python3 tests/test_cli.py        # installer behaviour (shim, .env, idempotency, uninstall)
```

Layout: the **provider** (`daimon_hermes/provider.py`) needs the Hermes runtime
(`agent.memory_provider`); the **CLI** (`daimon_hermes/cli.py`) is pure stdlib so
`daimon-hermes setup` runs in a bare shell. The package `__init__` guards the provider
import so the two never entangle.
