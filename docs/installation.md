# Installation

## Prerequisites

- **Rust toolchain** — stable Rust **1.75 or newer is required** (the workspace pins `rust-version = "1.75"`). Install via [rustup](https://rustup.rs/).
- **Claude Code** — `claude-code-scrollback` reads session files from `~/.claude/projects/`, which is populated by the [Claude Code](https://claude.com/claude-code) CLI. You don't need Claude Code running to browse history, but you do need it installed to have sessions to view.

## `cargo install` from git (recommended)

The fastest path — no clone, no manual copy. `cargo install --git` builds the binary in release mode and drops it into `$CARGO_HOME/bin` (typically `~/.cargo/bin/`, which is already on your `PATH` if you installed Rust via rustup).

```bash
cargo install --git https://github.com/pjh4993/claude-code-scrollback.git --bin claude-code-scrollback
```

To upgrade later, re-run the same command — `cargo install` overwrites in place.

Verify:

```bash
claude-code-scrollback --version
```

## From source

If you want to hack on the code or build against a specific commit:

```bash
git clone https://github.com/pjh4993/claude-code-scrollback.git
cd claude-code-scrollback
cargo build --release
cp target/release/claude-code-scrollback ~/.local/bin/
```

The release binary is around **2.6 MB** on macOS arm64 (well under PJH-53's 20 MB target) thanks to the workspace's `lto = "thin"` + `strip = true` release profile.

## Other install methods

- **`cargo install` from crates.io** — TBD (not yet published; the workspace uses path dependencies that need to be untangled before publish).
- **Homebrew** — TBD.
- **Prebuilt binaries / GitHub Releases** — TBD.

## Uninstall

If you installed via `cargo install --git` (the recommended path):

```bash
cargo uninstall claude-code-scrollback
```

If you built from source and copied the binary yourself, remove it from wherever you put it (e.g. `rm ~/.local/bin/claude-code-scrollback`).

Log files under the OS cache directory (see [contributing.md](./contributing.md#logs)) can be deleted manually if you want to clean up state.
