# Peter Commander

A dual-pane, keyboard-driven terminal file manager, in the tradition of
Norton Commander / Total Commander — built in Rust with
[ratatui](https://ratatui.rs).

## Features

- Dual-pane file browsing with full-path + selected-name titles
- File operations: open, rename, copy, move, mkdir, new file, delete, view, edit
- Pulldown menu bar with F-key shortcuts and a help screen (F1)
- Built-in command-line prompt with `cd` handling
- Ctrl+O to reveal/toggle the terminal
- Persistent settings (including a hide-hidden-files toggle), with save/cancel
- Quit confirmation and clean signal handling (SIGINT/SIGTERM/SIGHUP)

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
cp target/release/peter-commander ~/.local/bin/   # or anywhere on your PATH
```

### macOS

Same as "Other Linux" above — install Rust via `rustup` (or `brew install
rust`), then `cargo build --release`. All dependencies support macOS
terminals natively, no changes needed.

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
