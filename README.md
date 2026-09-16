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
cd packaging/arch
makepkg -si
```

This builds `peter-commander` from the tagged release source and installs
it system-wide as `/usr/bin/peter-commander`.

### From source

```sh
cargo build --release
./target/release/peter-commander
```

Requires a recent stable Rust toolchain (edition 2024).

## License

MIT — see [LICENSE](LICENSE).
