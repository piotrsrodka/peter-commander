#!/bin/sh
# Adds a `pc` alias for peter-commander to the caller's shell rc file,
# but only if `pc` isn't already taken by something on this machine.
set -eu

if command -v pc >/dev/null 2>&1; then
    echo "'pc' is already in use on this system ($(command -v pc)) — skipping alias setup." >&2
    exit 1
fi

case "${SHELL:-}" in
    */zsh) rc="$HOME/.zshrc" ;;
    */bash) rc="$HOME/.bashrc" ;;
    *)
        echo "Unrecognized shell '$SHELL' — add this line to your shell rc file manually:" >&2
        echo "  alias pc=peter-commander" >&2
        exit 1
        ;;
esac

if grep -q "alias pc=" "$rc" 2>/dev/null; then
    echo "'$rc' already defines an alias for 'pc' — skipping." >&2
    exit 1
fi

printf '\n# Added by peter-commander scripts/setup-alias.sh\nalias pc=peter-commander\n' >> "$rc"
echo "Added 'alias pc=peter-commander' to $rc. Restart your shell (or run 'source $rc') to use it."
