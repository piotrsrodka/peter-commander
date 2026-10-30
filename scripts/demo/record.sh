#!/usr/bin/env bash
# Records docs/demo.gif from docs/demo.tape in the current Omarchy theme.
# Usage: scripts/demo/record.sh   (from anywhere; needs vhs and python3)
#
# The palette comes from the foot config Omarchy generates for the current
# theme. Without Omarchy, vhs falls back to its default colors.
set -euo pipefail

cd "$(dirname "$0")/../.."
cargo build --release

foot_ini=$HOME/.local/state/omarchy/current/theme/foot.ini
tape=$(mktemp --suffix=.tape)
trap 'rm -f "$tape"' EXIT

if [[ -f $foot_ini ]]; then
    theme=$(python3 - "$foot_ini" <<'EOF'
import json, sys

names = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"]
colors = {}
for line in open(sys.argv[1]):
    key, sep, value = line.strip().partition("=")
    if sep:
        colors[key.strip()] = "#" + value.split()[-1].strip()

theme = {
    "background": colors["background"],
    "foreground": colors["foreground"],
    "selection": colors.get("selection-background", colors["foreground"]),
    "cursor": colors.get("cursor", colors["foreground"]),
}
for i, name in enumerate(names):
    theme[name] = colors[f"regular{i}"]
    theme["bright" + name.capitalize()] = colors[f"bright{i}"]
print(json.dumps(theme))
EOF
)
    echo "Set Theme $theme" > "$tape"
    echo "Theme: $(omarchy theme current 2>/dev/null || basename "$(dirname "$foot_ini")")"
fi
cat docs/demo.tape >> "$tape"

vhs "$tape"
