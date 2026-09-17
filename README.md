# Peter Commander

[![Rust](https://github.com/piotrsrodka/peter-commander/actions/workflows/rust.yml/badge.svg)](https://github.com/piotrsrodka/peter-commander/actions/workflows/rust.yml)

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

```sh
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander/packaging/arch
makepkg -si
```

Installs `peter-commander` system-wide as `/usr/bin/peter-commander`.
Requires sudo.

### Other Linux

**Step 1 — install Rust** (skip if `cargo --version` already works):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Step 2 — build and install:**

```sh
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander
./scripts/install.sh
```

No sudo needed. Installs to `~/.local/bin` and sets up a `pc` alias.

### macOS

**Step 1 — install Rust** (skip if `cargo --version` already works):

```sh
brew install rust
```

**Step 2 — build and install:**

```sh
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander
./scripts/install.sh
```

No sudo needed. Installs to `~/.local/bin` and sets up a `pc` alias.

---

Peter Commander is a TUI app and needs a real interactive terminal — it
won't run under a non-interactive script or with piped output.

## License

MIT — see [LICENSE](LICENSE).
