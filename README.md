# claude-code-scrollback
Terminal TUI for browsing, searching, and live-tailing Claude Code session history

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
