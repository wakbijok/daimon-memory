"""Tests for `daimon-hermes` setup CLI (the pip replacement for install.sh).

Verifies the installer behaviour against a throwaway HERMES_HOME:
  - writes the 2-line shim + plugin.yaml into <home>/plugins/daimon/
  - writes DAIMON_* config to <home>/.env (incl. remote endpoint + api key)
  - is idempotent and backs up .env on re-run
  - uninstall removes the shim
No real Hermes / backend needed (--no-activate; reachability is best-effort).
"""
import os
import sys
import tempfile
import types

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(_HERE))  # package root

# NO agent stub on purpose: the CLI must import WITHOUT the Hermes runtime — that's the
# whole point of decoupling provider (needs agent.*) from cli (pure stdlib). If this
# import needed agent.*, it would raise here and the decoupling has regressed.
from daimon_hermes import cli  # noqa: E402


def _run_setup(home, *extra):
    return cli.main([
        "setup",
        "--endpoint", "http://daimon.remote:8080",   # a REMOTE, non-default endpoint
        "--tenant", "11111111-1111-1111-1111-111111111111",
        "--namespace", "agent/lessons",
        "--api-key", "TESTKEY123",
        "--no-activate", "--yes",
        "--hermes-home", home, *extra,
    ])


def test_setup_writes_shim_and_env():
    with tempfile.TemporaryDirectory() as home:
        assert _run_setup(home) == 0
        shim = os.path.join(home, "plugins", "daimon", "__init__.py")
        assert os.path.exists(shim), "shim __init__.py written into plugins/daimon/"
        src = open(shim).read()
        assert "from daimon_hermes import" in src and "register" in src, "shim re-exports provider"
        assert os.path.exists(os.path.join(home, "plugins", "daimon", "plugin.yaml")), "plugin.yaml copied"
        env = open(os.path.join(home, ".env")).read()
        assert "DAIMON_ENDPOINT=http://daimon.remote:8080" in env  # remote endpoint configurable
        assert "DAIMON_TENANT=11111111-1111-1111-1111-111111111111" in env
        assert "DAIMON_NAMESPACE=agent/lessons" in env
        assert "DAIMON_API_KEY=TESTKEY123" in env


def test_setup_is_idempotent_and_backs_up():
    with tempfile.TemporaryDirectory() as home:
        _run_setup(home)
        _run_setup(home)
        env = open(os.path.join(home, ".env")).read()
        assert env.count("DAIMON_ENDPOINT=") == 1, "no duplicate env keys on re-run"
        assert os.path.exists(os.path.join(home, ".env.bak-daimon")), "backup created"


def test_uninstall_removes_shim():
    with tempfile.TemporaryDirectory() as home:
        _run_setup(home)
        assert cli.main(["uninstall", "--hermes-home", home, "--yes"]) == 0
        assert not os.path.exists(os.path.join(home, "plugins", "daimon"))


def test_missing_hermes_home_returns_1():
    rc = cli.main(["setup", "--endpoint", "http://x:8080", "--no-activate", "--yes",
                   "--hermes-home", os.path.join(tempfile.gettempdir(), "daimon-no-such-home-xyz")])
    assert rc == 1, "setup must fail (1) when HERMES_HOME does not exist"


def test_backup_is_pristine_and_taken_once():
    with tempfile.TemporaryDirectory() as home:
        with open(os.path.join(home, ".env"), "w") as f:
            f.write("USER_SECRET=keep\n")          # pre-existing user config
        _run_setup(home)
        _run_setup(home)                           # 2nd run must NOT re-backup
        bak = open(os.path.join(home, ".env.bak-daimon")).read()
        assert bak == "USER_SECRET=keep\n", f"backup must be the pristine pre-daimon .env; got {bak!r}"
        env = open(os.path.join(home, ".env")).read()
        assert "USER_SECRET=keep" in env and "DAIMON_ENDPOINT=" in env, "user key preserved + daimon added"


def test_rejects_newline_injection_in_value():
    with tempfile.TemporaryDirectory() as home:
        try:
            cli.main(["setup", "--endpoint", "http://x:8080", "--api-key", "good\nINJECT=evil",
                      "--no-activate", "--yes", "--hermes-home", home])
            raised = False
        except SystemExit:
            raised = True
        assert raised, "a value containing a newline must be rejected (env-injection guard)"


def test_activate_invokes_hermes():
    calls = []
    orig_which, orig_run = cli.shutil.which, cli.subprocess.run
    cli.shutil.which = lambda name: "/usr/bin/hermes"
    cli.subprocess.run = lambda argv, **kw: calls.append(argv)
    try:
        with tempfile.TemporaryDirectory() as home:
            rc = cli.main(["setup", "--endpoint", "http://x:8080", "--yes", "--hermes-home", home])
            assert rc == 0
            assert calls and calls[0][1:] == ["config", "set", "memory.provider", "daimon"], calls
    finally:
        cli.shutil.which, cli.subprocess.run = orig_which, orig_run


if __name__ == "__main__":
    test_setup_writes_shim_and_env()
    test_setup_is_idempotent_and_backs_up()
    test_uninstall_removes_shim()
    test_missing_hermes_home_returns_1()
    test_backup_is_pristine_and_taken_once()
    test_rejects_newline_injection_in_value()
    test_activate_invokes_hermes()
    print("PASS: daimon-hermes cli (setup/uninstall/missing-home/backup/inject-guard/activate)")
