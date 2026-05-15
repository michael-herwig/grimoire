# shellcheck shell=bash
# Source this to point `grim` at the manual rig:
#
#   source test/manual/scripts/env.sh
#
# Safe to source repeatedly. Uses an isolated GRIM_HOME under the rig so it
# never touches your real ~/.grimoire.

# shellcheck disable=SC2296  # ${(%):-%x} is the zsh path-of-sourced-file
# fallback (zsh has no BASH_SOURCE); this file is sourced from bash *or*
# zsh, so the zsh-only expansion is intentional under `shell=bash` lint.
_grim_env_script="${BASH_SOURCE[0]:-${(%):-%x}}"
_grim_manual_dir="$(cd "$(dirname "$_grim_env_script")/.." && pwd)"
_grim_repo_root="$(cd "$_grim_manual_dir/../.." && pwd)"

export GRIM_HOME="$_grim_manual_dir/.grim-home"
export GRIM_DEFAULT_REGISTRY="localhost:5050"
export GRIM_INSECURE_REGISTRIES="localhost:5050"

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
