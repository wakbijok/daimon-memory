"""daimon-memory provider for the NousResearch Hermes Agent (pip package ``daimon-hermes``).

Two surfaces with different dependency footprints, kept decoupled on purpose:

* ``daimon_hermes.cli`` — the ``daimon-hermes`` console-script (install/configure). Pure
  stdlib, NO Hermes dependency, so ``daimon-hermes setup`` runs in a bare shell right
  after ``pip install`` — before Hermes is even configured.
* ``daimon_hermes.provider`` — ``DaimonMemoryProvider``, which subclasses
  ``agent.memory_provider.MemoryProvider``. That base only exists inside a running Hermes
  process, so the import is GUARDED: outside Hermes it's simply unavailable (the CLI still
  works); inside Hermes the plugin shim (`from daimon_hermes import ... register`) resolves it.
"""
from .cli import main  # noqa: F401  -- Hermes-independent console-script entry point

try:
    import agent.memory_provider  # noqa: F401  -- the Hermes runtime base; absent in a bare CLI shell
except ImportError:
    # Running standalone (e.g. `daimon-hermes setup` before/outside Hermes). The provider
    # can't load without its base; expose clear stand-ins. The CLI surface is unaffected.
    DaimonMemoryProvider = None  # type: ignore

    def register(ctx):  # type: ignore
        raise RuntimeError(
            "daimon_hermes provider requires the Hermes agent runtime "
            "(agent.memory_provider); it can only load inside a Hermes session."
        )
else:
    # Inside Hermes the base IS present, so import the provider for real and DO NOT swallow
    # its errors — a typo/regression in provider.py must surface with its true traceback,
    # not be masked as "requires Hermes runtime" (which sends debugging down the wrong path).
    from .provider import DaimonMemoryProvider, register  # noqa: F401

__all__ = ["main", "DaimonMemoryProvider", "register"]
