# Peter Commander

A dual-pane, keyboard-driven terminal file manager, in the tradition of
Norton Commander / Total Commander — built in Rust with ratatui
(https://ratatui.rs).

## Features

- Dual-pane file browsing
- File operations: open, rename, copy, move, mkdir, new file, delete, view, edit
- Built-in command-line prompt, with `cd` support
- Quick terminal access (Ctrl+O)
- Settings that persist across sessions (e.g. show/hide hidden files)

## Installing

### Arch Linux / Omarchy

A `PKGBUILD` is provided in `packaging/arch/`:

```sh
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander/packaging/arch
makepkg -si
```

This builds `peter-commander` from the tagged release source and installs
it system-wide as `/usr/bin/peter-commander`.

### Other Linux

Install Rust if you don't already have it, then build from source:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander
cargo build --release
cp target/release/peter-commander ~/.local/bin/
```

(`~/.local/bin` just needs to be on your `PATH` — pick any directory that is.)

### macOS

```sh
brew install rust
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander
cargo build --release
cp target/release/peter-commander ~/.local/bin/
```

### Optional: a shorter `pc` command

After installing (any platform above), you can run:

```sh
./scripts/setup-alias.sh
```

This adds `alias pc=peter-commander` to your shell rc file (`.bashrc` or
`.zshrc`) — but only if `pc` isn't already used by something else on your
system. It checks your actual machine at run time rather than relying on
a package database, since that's what actually matters and no single
database covers every distro anyway.

### From source (general)

```sh
cargo build --release
./target/release/peter-commander
```

Requires a recent stable Rust toolchain (edition 2024). Note this is a TUI
app that needs a real interactive terminal (raw-mode support) — it won't
run under a non-interactive script or with piped output.

## License

MIT — see [LICENSE](LICENSE).
