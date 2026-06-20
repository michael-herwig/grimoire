from src.assertions import (
    assert_dir_exists,
    assert_not_exists,
    assert_path_exists,
    assert_symlink_exists,
)
from src.runner import GrimRunner, current_platform

# NOTE: src.helpers is intentionally NOT re-exported here.
# Importing src.helpers at package-init time would transitively pull in
# src.registry (via "from src.registry import ..."), capturing REGISTRY_HOST
# before pytest_configure has a chance to set GRIM_TEST_REGISTRY_HOST.
# Tests that need src.helpers must import it directly:
#   from src.helpers import make_artifact, PROJECT_ROOT

__all__ = [
    "GrimRunner",
    "assert_dir_exists",
    "assert_not_exists",
    "assert_path_exists",
    "assert_symlink_exists",
    "current_platform",
]
