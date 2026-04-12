# Installation

## Prerequisites

- **Rust toolchain** — stable Rust (1.75+ recommended). Install via [rustup](https://rustup.rs/).
- **Claude Code** — `claude-code-scrollback` reads session files from `~/.claude/projects/`, which is populated by the [Claude Code](https://claude.com/claude-code) CLI. You don't need Claude Code running to browse history, but you do need it installed to have sessions to view.

## From source (currently the only supported method)

Clone the repo and build a release binary:

```bash
git clone https://github.com/pjh4993/claude-code-scrollback.git
cd claude-code-scrollback
cargo build --release
```

The binary lands at `target/release/claude-code-scrollback`. Copy it somewhere on your `PATH`:

```bash
cp target/release/claude-code-scrollback ~/.local/bin/
```

Verify:

```bash
claude-code-scrollback --version
```

## Other install methods

- **`cargo install` from crates.io** — TBD (not yet published).
- **Homebrew** — TBD.
- **Prebuilt binaries / GitHub Releases** — TBD.

## Uninstall

Remove the binary you copied (e.g. `rm ~/.local/bin/claude-code-scrollback`). Log files under the OS cache directory (see [contributing.md](./contributing.md#logs)) can be deleted manually if you want to clean up state.
