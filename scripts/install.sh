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
cp target/release/peter-commander "$bin_dir/"
echo "Installed peter-commander to $bin_dir/peter-commander"

case ":${PATH:-}:" in
    *":$bin_dir:"*) ;;
    *)
        echo "Note: $bin_dir isn't on your PATH. Add this to your shell rc file:"
        echo "  export PATH=\"$bin_dir:\$PATH\""
        ;;
esac

"$script_dir/setup-alias.sh"
