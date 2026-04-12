# claude-code-scrollback

A terminal TUI for browsing, searching, and live-tailing [Claude Code](https://claude.com/claude-code) session history.

`claude-code-scrollback` reads the JSONL session files that Claude Code writes under `~/.claude/projects/` and presents them as a navigable, searchable transcript — so you can go back to any past conversation without scrolling your terminal emulator or hunting through raw files.

## Features

- **Session picker** ranked by affinity to the directory you launch from, so the project you're working on surfaces first.
- **Transcript viewer** with vim-style navigation (`j`/`k`, `g`/`G`, `Ctrl-d`/`Ctrl-u`).
- **Live tail mode** (`--live`) for watching an active session update in real time.
- **Open-by-path or by-id** — point at a session id prefix, or hand it any `.jsonl` file on disk.
- **Rotating file logs** kept out of stderr so the TUI stays clean.

## Documentation

- [**Installation**](./docs/installation.md) — prerequisites and building from source.
- [**Quick start**](./docs/quickstart.md) — first run, keybindings, common flags.
- [**Contributing**](./docs/contributing.md) — dev loop, pre-commit hook, CI checks, log file locations.

## License

See [LICENSE](./LICENSE).
