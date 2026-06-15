"""``daimon-hermes`` — installer/configurator CLI for the daimon-memory Hermes provider.

Replaces the old bash ``install.sh``. Because ``pip install`` runs no install-time
code, configuration lives here instead: ``pip install daimon-hermes`` ships the code +
this console-script, then ``daimon-hermes setup`` wires it into Hermes.

  daimon-hermes setup [--endpoint URL] [--tenant UUID] [--namespace NS]
                      [--api-key KEY] [--activate/--no-activate] [--yes]
  daimon-hermes uninstall [--yes]

Pure stdlib — no third-party deps — so it runs identically on Linux/macOS/Windows
(no Git Bash needed), unlike the bash installer it replaces. All file writes are atomic
(temp + os.replace) and .env edits are idempotent, value-validated, and backed up.
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from importlib import resources
from urllib.parse import urlparse

DEV_TENANT = "00000000-0000-0000-0000-0000000000d1"
DEFAULT_ENDPOINT = "http://localhost:8080"
DEFAULT_NAMESPACE = "agent/lessons"

_SHIM_MARKER = "# Managed by `daimon-hermes setup`"
# The shim re-exports the pip-installed provider. It explicitly names BOTH tokens Hermes
# text-scans for ("register_memory_provider", "MemoryProvider") so discovery never relies
# on the incidental substring in "DaimonMemoryProvider".
_SHIM = f"""{_SHIM_MARKER} — do not edit by hand.
# Thin shim: the daimon-memory provider ships in the pip package `daimon-hermes`
# (import path: daimon_hermes), so `pip install -U daimon-hermes` upgrades it with no
# re-setup and no file drift. Hermes discovers providers by directory and text-scans for
# `register_memory_provider` / `MemoryProvider`; this file re-exports the real ones.
from daimon_hermes import DaimonMemoryProvider, register  # noqa: F401
"""

# Fallback plugin.yaml — kept in sync with daimon_hermes/plugin.yaml INCLUDING the hooks
# block, so a wheel that somehow lacks the packaged file still registers the custom hooks.
_PLUGIN_YAML_FALLBACK = """name: daimon
version: 0.1.0
description: "daimon-memory — shared cross-tool context plane."
pip_dependencies:
  - httpx
requires_env:
  - DAIMON_ENDPOINT
hooks:
  - on_memory_write
  - on_session_end
"""


def _hermes_home(arg: str | None) -> str:
    return os.path.abspath(
        arg or os.environ.get("HERMES_HOME") or os.path.expanduser("~/.hermes")
    )


def _ask(prompt: str, default: str, assume_yes: bool) -> str:
    if assume_yes or not (sys.stdin.isatty() and sys.stdout.isatty()):
        return default
    try:
        ans = input(f"  {prompt} [{default}]: ").strip()
    except (EOFError, KeyboardInterrupt):
        ans = ""
    return ans or default


def _confirm(prompt: str, assume_yes: bool) -> bool:
    if assume_yes:
        return True
    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        return False  # non-interactive without --yes: default to NO for destructive ops
    try:
        return input(f"  {prompt} [y/N]: ").strip().lower() in ("y", "yes")
    except (EOFError, KeyboardInterrupt):
        return False


def _validate(name: str, value: str) -> str:
    """Reject newline/CR so a value can't inject extra .env lines (or corrupt the file)."""
    if "\n" in value or "\r" in value:
        raise SystemExit(f"ERROR: {name} must not contain newlines: {value!r}")
    return value


def _atomic_write(path: str, content: str, mode: int | None = None) -> None:
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        f.write(content)
    if mode is not None:
        try:
            os.chmod(tmp, mode)
        except OSError:
            pass
    os.replace(tmp, path)  # atomic on POSIX and same-volume Windows


def _write_env(env_path: str, pairs: list[tuple[str, str]]) -> None:
    """Idempotently add missing key=value lines; rebuild + atomic-replace (never append-in-place)."""
    raw = ""
    if os.path.exists(env_path):
        with open(env_path, encoding="utf-8") as f:
            raw = f.read()
    present = {
        line.split("=", 1)[0]
        for line in raw.splitlines()
        if "=" in line and not line.lstrip().startswith("#")
    }
    additions = []
    for key, value in pairs:
        if key in present:
            print(f"  = {key} (already set; edit {env_path} to change)")
        else:
            additions.append(f"{key}={value}")
            print(f"  + {key}")
    if not additions:
        return
    new = raw
    if new and not new.endswith("\n"):
        new += "\n"  # never glue a new key onto a no-trailing-newline last line
    new += "\n".join(additions) + "\n"
    _atomic_write(env_path, new, mode=0o600)


def _reachable(endpoint: str) -> bool:
    import urllib.request

    if urlparse(endpoint).scheme not in ("http", "https"):
        return False  # don't let file://, ftp://, etc. through urlopen
    try:
        with urllib.request.urlopen(endpoint.rstrip("/") + "/readyz", timeout=4) as r:
            return 200 <= getattr(r, "status", 0) < 300
    except Exception:
        return False


def _plugin_yaml_text() -> str:
    try:
        return resources.files("daimon_hermes").joinpath("plugin.yaml").read_text(encoding="utf-8")
    except Exception:
        return _PLUGIN_YAML_FALLBACK


def do_setup(args: argparse.Namespace) -> int:
    home = _hermes_home(args.hermes_home)
    if not os.path.isdir(home):
        print(f"ERROR: HERMES_HOME not found: {home}", file=sys.stderr)
        return 1

    endpoint = _validate("endpoint", args.endpoint or _ask(
        "daimon-memory endpoint (URL of your daimon-memory server)", DEFAULT_ENDPOINT, args.yes))
    tenant = _validate("tenant", args.tenant or _ask(
        "Tenant ID (memory space — must match your other tools)", DEV_TENANT, args.yes))
    namespace = _validate("namespace", args.namespace or _ask(
        "Default namespace (where Hermes files captured memories)", DEFAULT_NAMESPACE, args.yes))
    api_key = _validate("api-key", args.api_key) if args.api_key else ""

    print(f"  {'OK reachable' if _reachable(endpoint) else 'WARN not reachable yet (fine — recall is best-effort)'}: {endpoint}")

    # 1) install the plugin shim + plugin.yaml (atomic; never clobber a foreign file)
    dest = os.path.join(home, "plugins", "daimon")
    os.makedirs(dest, exist_ok=True)
    shim_path = os.path.join(dest, "__init__.py")
    if os.path.exists(shim_path):
        existing = open(shim_path, encoding="utf-8", errors="replace").read()
        if _SHIM_MARKER not in existing and not _confirm(
            f"{shim_path} exists and is not managed by daimon-hermes; overwrite?", args.yes):
            print("Aborted: refusing to overwrite a non-managed plugin file.")
            return 1
    _atomic_write(shim_path, _SHIM)
    _atomic_write(os.path.join(dest, "plugin.yaml"), _plugin_yaml_text())
    print(f"installed plugin shim -> {dest}")

    # 2) record config in .env (back up the PRISTINE non-empty file once, at 0600)
    env_path = os.path.join(home, ".env")
    backup = env_path + ".bak-daimon"
    if os.path.exists(env_path) and os.path.getsize(env_path) > 0 and not os.path.exists(backup):
        shutil.copyfile(env_path, backup)
        try:
            os.chmod(backup, 0o600)  # copyfile does NOT preserve perms — secrets must not be world-readable
        except OSError:
            pass
        print(f"backed up existing .env -> {backup}")
    print(f"configuring {env_path}:")
    pairs = [("DAIMON_ENDPOINT", endpoint), ("DAIMON_TENANT", tenant), ("DAIMON_NAMESPACE", namespace)]
    if api_key:
        pairs.append(("DAIMON_API_KEY", api_key))
    _write_env(env_path, pairs)
    if api_key and os.name == "nt":
        print("  WARN: on Windows, file permissions on .env are not enforced — the API key is "
              "stored in plaintext readable by your account. Prefer a per-user secure store.")

    # 3) activate (opt-in; default on)
    if args.activate:
        hermes = shutil.which("hermes")
        if not hermes:
            print("WARN: `hermes` not on PATH — activate manually: hermes config set memory.provider daimon")
        else:
            try:
                subprocess.run([hermes, "config", "set", "memory.provider", "daimon"],
                               check=True, capture_output=True)
                print("ACTIVATED: memory.provider=daimon (restart your Hermes session)")
            except subprocess.CalledProcessError as e:
                err = (e.stderr or b"").decode("utf-8", "replace").strip()
                print(f"WARN: `hermes config set` failed: {err or e}")
    else:
        print("Installed (not activated). Activate with: hermes config set memory.provider daimon")
    return 0


def do_uninstall(args: argparse.Namespace) -> int:
    home = _hermes_home(args.hermes_home)
    dest = os.path.join(home, "plugins", "daimon")
    if not os.path.isdir(dest):
        print(f"nothing to remove at {dest}")
        return 0
    shim_path = os.path.join(dest, "__init__.py")
    managed = os.path.exists(shim_path) and _SHIM_MARKER in open(
        shim_path, encoding="utf-8", errors="replace").read()
    if not managed and not _confirm(f"{dest} is not a daimon-hermes-managed plugin; remove anyway?", args.yes):
        print("Aborted: refusing to remove a directory daimon-hermes did not create.")
        return 1
    if not args.yes and not _confirm(f"Remove {dest}?", args.yes):
        print("Aborted.")
        return 0
    shutil.rmtree(dest)
    print(f"removed: {dest}")
    print(f"Note: DAIMON_* env vars, .env, and {os.path.basename(home)}/.env.bak-daimon (if any, "
          "may contain your API key) were left untouched — remove them manually if desired.")
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="daimon-hermes", description="Connect Hermes to daimon-memory.")
    sub = p.add_subparsers(dest="cmd")

    sp = sub.add_parser("setup", help="install + configure the daimon provider into Hermes")
    sp.add_argument("--endpoint", help="daimon-memory base URL (remote or local)")
    sp.add_argument("--tenant", help="tenant UUID")
    sp.add_argument("--namespace", help="default capture namespace")
    sp.add_argument("--api-key", dest="api_key", help="bearer token (if daimon-mcp auth is enabled)")
    grp = sp.add_mutually_exclusive_group()
    grp.add_argument("--activate", dest="activate", action="store_true", default=True,
                     help="set memory.provider=daimon (default)")
    grp.add_argument("--no-activate", dest="activate", action="store_false",
                     help="install only, don't switch the active provider")
    sp.add_argument("-y", "--yes", action="store_true", help="non-interactive; accept defaults/overwrites")
    sp.add_argument("--hermes-home", help="override $HERMES_HOME (default ~/.hermes)")
    sp.set_defaults(func=do_setup)

    up = sub.add_parser("uninstall", help="remove the daimon provider shim from Hermes")
    up.add_argument("-y", "--yes", action="store_true", help="non-interactive; skip confirmation")
    up.add_argument("--hermes-home", help="override $HERMES_HOME (default ~/.hermes)")
    up.set_defaults(func=do_uninstall)

    args = p.parse_args(argv)
    if not getattr(args, "cmd", None):
        p.print_help()
        return 2
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
