#!/bin/sh
# Adds a `pc` alias for peter-commander to the caller's shell rc file,
# but only if `pc` isn't already taken by something on this machine.
set -eu

if command -v pc >/dev/null 2>&1; then
    echo "Skipping 'pc' alias: already in use on this system ($(command -v pc))."
    exit 0
fi

case "${SHELL:-}" in
    */zsh) rc="$HOME/.zshrc" ;;
    */bash) rc="$HOME/.bashrc" ;;
    *)
        echo "Skipping 'pc' alias: unrecognized shell '$SHELL'. Add this line to your shell rc file yourself if you want it:"
        echo "  alias pc=peter-commander"
        exit 0
        ;;
esac

if grep -q "alias pc=" "$rc" 2>/dev/null; then
    echo "Skipping 'pc' alias: '$rc' already defines one."
    exit 0
fi

printf '\n# Added by peter-commander scripts/setup-alias.sh\nalias pc=peter-commander\n' >> "$rc"
echo "Added 'alias pc=peter-commander' to $rc. Restart your shell (or run 'source $rc') to use it."
