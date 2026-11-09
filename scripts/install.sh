#!/bin/sh
# Builds Peter Commander and installs it to ~/.local/bin (no sudo needed),
# then sets up the optional `pc` alias.
set -eu

if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust (cargo) not found. Install it first, then re-run this script:" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    echo "(or 'brew install rust' on macOS)" >&2
    exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

cargo build --release

bin_dir="$HOME/.local/bin"
mkdir -p "$bin_dir"
# Through a temp file and a rename: copying straight over a binary that's
# running (pce as $EDITOR, say) fails with "Text file busy".
cp target/release/peter-commander "$bin_dir/.peter-commander.new"
mv -f "$bin_dir/.peter-commander.new" "$bin_dir/peter-commander"
echo "Installed peter-commander to $bin_dir/peter-commander"
# pce, the editor alone (usable as $EDITOR): a link, as the binary notices
# the name it was run under. Only ours is replaced, never another program.
if [ ! -e "$bin_dir/pce" ] || [ -L "$bin_dir/pce" ]; then
    ln -sf peter-commander "$bin_dir/pce"
    echo "Linked $bin_dir/pce (the editor alone) to it"
else
    echo "Warning: $bin_dir/pce is another program; left it alone (no pce link)." >&2
fi

case ":${PATH:-}:" in
    *":$bin_dir:"*) ;;
    *)
        echo "Note: $bin_dir isn't on your PATH. Add this to your shell rc file:"
        echo "  export PATH=\"$bin_dir:\$PATH\""
        ;;
esac

"$script_dir/setup-alias.sh"
