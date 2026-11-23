"""Relay's sitecustomize: installs the Auto-mode guard (relay_guard.py), then
loads the sitecustomize it shadows, if there is one. Relay puts this folder
first on PYTHONPATH for commands in Auto mode."""

import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))

import relay_guard  # noqa: E402

relay_guard.install()

# The program's own sitecustomize (a venv's, a distribution's), found past ours.
# Our folder leaves sys.path: nothing else is imported from it.
_mine = sys.modules.pop("sitecustomize", None)
sys.path[:] = [p for p in sys.path if os.path.normcase(os.path.abspath(p or ".")) != os.path.normcase(_here)]
try:
    import sitecustomize  # noqa: F401,E402
except ImportError:
    if _mine is not None:
        sys.modules["sitecustomize"] = _mine
