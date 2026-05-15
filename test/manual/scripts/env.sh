# shellcheck shell=bash
# Source this to point `grim` at the manual rig:
#
#   source test/manual/scripts/env.sh
#
# Safe to source repeatedly. Uses an isolated GRIM_HOME under the rig so it
# never touches your real ~/.grimoire.

_grim_env_script="${BASH_SOURCE[0]:-${(%):-%x}}"
_grim_manual_dir="$(cd "$(dirname "$_grim_env_script")/.." && pwd)"
_grim_repo_root="$(cd "$_grim_manual_dir/../.." && pwd)"

export GRIM_HOME="$_grim_manual_dir/.grim-home"
export GRIM_DEFAULT_REGISTRY="localhost:5000"
export GRIM_INSECURE_REGISTRIES="localhost:5000"

case ":$PATH:" in
*":$_grim_repo_root/test/bin:"*) ;;
*) export PATH="$_grim_repo_root/test/bin:$PATH" ;;
esac

{
	echo "grimoire manual env:"
	echo "  GRIM_HOME=$GRIM_HOME"
	echo "  GRIM_DEFAULT_REGISTRY=$GRIM_DEFAULT_REGISTRY"
	echo "  GRIM_INSECURE_REGISTRIES=$GRIM_INSECURE_REGISTRIES"
	echo "  grim -> $(command -v grim 2>/dev/null || echo '(not built yet — run bootstrap.sh)')"
} >&2

unset _grim_env_script _grim_manual_dir _grim_repo_root
