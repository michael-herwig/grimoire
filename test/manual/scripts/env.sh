# shellcheck shell=bash
# Source this to point `grim` at the manual rig:
#
#   source test/manual/scripts/env.sh
#
# Safe to source repeatedly. Uses an isolated GRIM_HOME under the rig so it
# never touches your real ~/.grimoire.

# This file is sourced from bash *or* zsh. bash exposes BASH_SOURCE; zsh
# does not, so fall back to the zsh `%x` prompt expansion (the path of the
# sourced file). The zsh-only `${(%):-%x}` is wrapped in `eval` so bash's
# parser — and shfmt, which lints this file as bash — never sees the zsh
# syntax; that branch only ever runs under zsh.
if [ -n "${BASH_SOURCE[0]:-}" ]; then
    _grim_env_script="${BASH_SOURCE[0]}"
else
    eval '_grim_env_script="${(%):-%x}"'
fi
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
