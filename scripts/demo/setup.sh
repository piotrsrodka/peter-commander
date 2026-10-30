#!/usr/bin/env bash
# Builds a made-up home directory for recording docs/demo.gif with vhs.
# Usage: scripts/demo/setup.sh <dir>   (the directory is wiped and recreated)
set -euo pipefail

root=${1:?usage: demo-setup.sh <dir>}
rm -rf "$root"
mkdir -p "$root"
cd "$root"

h=peter
mkdir -p $h/Projects/starfield/{src,data,docs,examples,tests} $h/Projects/retro-games \
    $h/Documents $h/Music/vangelis $h/Photos/2026-tatry $h/Backup \
    config/peter-commander data

# Settings for the recording: built-in F3 preview, F8 to trash.
cat > config/peter-commander/settings <<'EOF'
internal_preview=true
hide_hidden_files=true
use_trash=true
classic_style=false
start_left_in_cwd=true
EOF

cd $h/Projects/starfield
cat > README.md <<'EOF'
# Starfield

A tiny star catalog viewer for the terminal.

## Features

- Loads the Hipparcos catalog subset from data/
- Plots stars by right ascension and declination
- Colors stars by B-V index, from blue giants to red dwarfs
- Zoom into constellations with + and -

## Running

    cargo run --release -- data/catalog.csv

## Roadmap

- [x] Catalog loader
- [x] Equatorial projection
- [ ] Constellation lines
- [ ] Planets for a given date
EOF
cat > Cargo.toml <<'EOF'
[package]
name = "starfield"
version = "0.3.0"
edition = "2024"

[dependencies]
ratatui = "0.30"
csv = "1"
EOF
cat > src/main.rs <<'EOF'
mod catalog;
mod projection;

fn main() -> anyhow::Result<()> {
    let stars = catalog::load("data/catalog.csv")?;
    projection::draw(&stars)
}
EOF
printf 'pub fn load() {}\n' > src/catalog.rs
printf 'pub fn draw() {}\n' > src/projection.rs
{
    echo "hip,name,ra,dec,mag,bv"
    echo "32349,Sirius,101.287,-16.716,-1.46,0.00"
    echo "30438,Canopus,95.988,-52.696,-0.74,0.15"
    echo "69673,Arcturus,213.915,19.182,-0.05,1.24"
    echo "91262,Vega,279.234,38.784,0.03,0.00"
    echo "24608,Capella,79.172,45.998,0.08,0.80"
    echo "24436,Rigel,78.634,-8.202,0.13,-0.03"
    echo "37279,Procyon,114.825,5.225,0.34,0.42"
    echo "27989,Betelgeuse,88.793,7.407,0.42,1.85"
} > data/catalog.csv
head -c 2400000 /dev/urandom > data/hipparcos.bin
printf 'Equatorial to screen coordinates.\n' > docs/projection.md
printf 'warning: unused variable `zoom`\n' > build.log
printf 'target/\n' > .gitignore
printf 'MIT License\n\nCopyright (c) 2026 Peter\n' > LICENSE
printf 'max_width = 100\n' > rustfmt.toml
printf '# Changelog\n\n## 0.3.0\n- B-V colors\n' > CHANGELOG.md
for dep in anyhow cassowary crossterm csv itoa memchr ratatui serde unicode-width; do
    printf '[[package]]\nname = "%s"\nversion = "1.0.%d"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n\n' \
        $dep $((RANDOM % 90))
done > Cargo.lock
printf 'fn main() {}\n' > examples/orion.rs
printf '#[test]\nfn loads() {}\n' > tests/catalog.rs

cd "$root/$h/Projects/retro-games"
for f in prince-of-persia.zip doom-shareware.zip lemmings.zip; do
    head -c $((RANDOM * 40)) /dev/urandom > $f
done

cd "$root/$h/Documents"
printf 'Wroclaw, autumn 2026\n' > notes.txt
printf '# CV\n' > cv.md
head -c 184000 /dev/urandom > invoice-2026-10.pdf
head -c 92000 /dev/urandom > telescope-manual.pdf

cd "$root/$h/Music/vangelis"
for t in 01-chariots-of-fire 02-blade-runner-blues 03-alpha; do
    head -c $((RANDOM * 150)) /dev/urandom > $t.mp3
done

cd "$root/$h/Photos/2026-tatry"
for n in 0412 0413 0417 0421 0438; do
    head -c $((RANDOM * 90)) /dev/urandom > IMG_$n.jpg
done

# Spread the modification times so the date column is not all "now".
d=0
find "$root/$h" -mindepth 1 | sort | while read -r p; do
    d=$(( (d + 7) % 300 ))
    touch -d "-$d days" "$p"
done
