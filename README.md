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

A `PKGBUILD` is provided in `packaging/arch/` — Rust is pulled in
automatically as a build dependency, no separate install step needed:

```sh
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander/packaging/arch
makepkg -si
```

This builds `peter-commander` from the tagged release source and installs
it system-wide as `/usr/bin/peter-commander`.

### Other Linux (build from source)

No sudo needed — everything installs under your home directory.

**Step 1 — install Rust.** Skip this if `cargo --version` already works
(a recent stable toolchain — edition 2024 — is required).

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Step 2 — build and install Peter Commander:**

```sh
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander
./scripts/install.sh
```

This builds the release binary, copies it to `~/.local/bin`, and sets up
a short `pc` alias — automatically, in one step. It warns you if
`~/.local/bin` isn't on your `PATH` yet, and only adds the `pc` alias if
that name isn't already used by something else on your system (checked
on your actual machine at run time, since no package database covers
every distro anyway — it's always safe to run and never overwrites an
existing `pc`).

### macOS (build from source)

No sudo needed — everything installs under your home directory.

**Step 1 — install Rust.** Skip this if `cargo --version` already works.

```sh
brew install rust
```

**Step 2 — build and install Peter Commander:**

```sh
git clone https://github.com/piotrsrodka/peter-commander.git
cd peter-commander
./scripts/install.sh
```

Same as above: builds, installs to `~/.local/bin`, and sets up the `pc`
alias automatically.

---

Note this is a TUI app that needs a real interactive terminal (raw-mode
support) — it won't run under a non-interactive script or with piped
output.

## License

MIT — see [LICENSE](LICENSE).
