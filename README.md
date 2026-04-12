# claude-code-scrollback
Terminal TUI for browsing, searching, and live-tailing Claude Code session history

## Debugging / logs

The app writes rotating daily log files to the OS cache directory so the TUI
never has to share stderr with your logs:

| Platform | Default path |
|----------|--------------|
| macOS    | `~/Library/Caches/claude-code-scrollback/logs/ccs.log.<YYYY-MM-DD>` |
| Linux    | `$XDG_CACHE_HOME/claude-code-scrollback/logs/ccs.log.<YYYY-MM-DD>` (falls back to `~/.cache/...`) |
| Windows  | `%LOCALAPPDATA%\claude-code-scrollback\logs\ccs.log.<YYYY-MM-DD>` |

Control verbosity and format with CLI flags or the `RUST_LOG` env var. The
CLI flag wins if both are set:

```bash
claude-code-scrollback --log-level debug
claude-code-scrollback --log-level 'ccs_core=trace,info'
claude-code-scrollback --log-format json
RUST_LOG=debug claude-code-scrollback
```

Tail the live log in a second pane while the TUI is running:

```bash
tail -f ~/Library/Caches/claude-code-scrollback/logs/ccs.log.*
```

## Contributing

This repo uses [`pre-commit`](https://pre-commit.com/) to run `cargo fmt` and
`cargo clippy` before every commit. After cloning, install the hook once:

```bash
brew install pre-commit   # or: pip install pre-commit
pre-commit install
```

CI runs the same checks (`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo check --workspace --all-targets`) on every push and pull request.
