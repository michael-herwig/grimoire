from __future__ import annotations

from pathlib import Path

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

# test/src/helpers.py -> test/src -> test -> project root
PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
