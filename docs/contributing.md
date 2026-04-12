# Contributing

Thanks for your interest in hacking on `claude-code-scrollback`. This document covers the local dev loop and the checks CI runs.

## Dev loop

```bash
# Format
cargo fmt --all

# Lint (CI runs with -D warnings — treat any clippy warning as a build failure)
cargo clippy --workspace --all-targets -- -D warnings

# Type-check the whole workspace
cargo check --workspace --all-targets

# Run the TUI against a fixture file without touching ~/.claude/
cargo run -p ccs-cli -- ./path/to/sample.jsonl
```

The workspace has three crates:

- `ccs-core` — session discovery, JSONL parsing, metadata.
- `ccs-tui` — ratatui-based UI (picker + transcript viewer).
- `ccs-cli` — thin binary that wires the two together and handles argv/logging.

## Pre-commit hook

This repo uses [`pre-commit`](https://pre-commit.com/) to run `cargo fmt` and `cargo clippy` before every commit. After cloning, install the hook once:

```bash
brew install pre-commit   # or: pip install pre-commit
pre-commit install
```

## CI

Every push and pull request runs:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo check --workspace --all-targets`

`cargo test` runs on PRs once the `ready` label is applied (see `.github/workflows/`).

## Logs

The app writes rotating daily log files to the OS cache directory so the TUI never has to share stderr with your logs:

| Platform | Default path |
|----------|--------------|
| macOS    | `~/Library/Caches/claude-code-scrollback/logs/ccs.log.<YYYY-MM-DD>` |
| Linux    | `$XDG_CACHE_HOME/claude-code-scrollback/logs/ccs.log.<YYYY-MM-DD>` (falls back to `~/.cache/...`) |
| Windows  | `%LOCALAPPDATA%\claude-code-scrollback\logs\ccs.log.<YYYY-MM-DD>` |

Control verbosity and format with CLI flags or the `RUST_LOG` env var. The CLI flag wins if both are set:

```bash
claude-code-scrollback --log-level debug
claude-code-scrollback --log-level 'ccs_core=trace,info'
claude-code-scrollback --log-format json
RUST_LOG=debug claude-code-scrollback
```

`--log-level` accepts any [`tracing_subscriber::EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) directive.

Tail the live log in a second pane while the TUI is running:

```bash
tail -f ~/Library/Caches/claude-code-scrollback/logs/ccs.log.*
```

## Pull requests

- Keep PRs focused — one concern per PR.
- Write commit messages that explain *why*, not *what*. The diff shows the what.
- Add tests when fixing a bug or adding behavior that's easy to regress. Parsing, keymap, and session-discovery code all have unit tests already — follow the existing patterns.
- If your change touches user-visible behavior (new flags, new keybindings, changed output paths), update the relevant file under `docs/`.
