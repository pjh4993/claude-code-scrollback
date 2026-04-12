# Quick start

This guide assumes you've already [installed](./installation.md) `claude-code-scrollback` and have at least one Claude Code session on disk under `~/.claude/projects/`.

## Launching

```bash
# Open the session picker, ranked by affinity to the current directory
claude-code-scrollback

# Live-tail the most recent active session, skipping the picker
claude-code-scrollback --live

# Open a specific session by id (or id prefix)
claude-code-scrollback 7f3c1a9b

# Open a specific .jsonl file from anywhere on disk
claude-code-scrollback ./fixtures/sample.jsonl
```

Run `claude-code-scrollback --help` for the full flag list.

## Picker

The picker lists every session under `~/.claude/projects/`, ranked so that sessions from the directory you launched in bubble to the top.

| Key | Action |
|-----|--------|
| `j` / `↓` | Move down |
| `k` / `↑` | Move up |
| `g` / `Home` | Jump to top |
| `G` / `End` | Jump to bottom |
| `/` | Enter search |
| `Enter` | Open selected session |
| `q` | Quit |

While in search mode: type to filter, `Enter` to commit, `Esc` to cancel, `Backspace` to delete a character.

## Transcript viewer

Once a session is open, you're in the transcript viewer.

| Key | Action |
|-----|--------|
| `j` / `↓` | Scroll down one line |
| `k` / `↑` | Scroll up one line |
| `Ctrl-d` | Half-page down |
| `Ctrl-u` | Half-page up |
| `g g` | Jump to top |
| `G` / `End` | Jump to bottom |
| `Home` | Jump to top |
| `q` / `Esc` | Back to picker (or quit in `--live` mode) |

## Next steps

- Hacking on the code or want to tail the log file? See [contributing.md](./contributing.md).
