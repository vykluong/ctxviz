# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Chats and the default state dir now follow the Cursor CLI config dir (`$CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else `~/.cursor`) instead of always using `~/.cursor`. With a relocated config dir, the status line used to show only the payload row (no breakdown or trend) and created `~/.cursor/ctxviz`, and the TUI and `snapshot` could not find chats or picked a stale one with `--latest`. A relative path in either variable is rejected.
- `ctxviz doctor` prints the resolved config, chats, and state dirs.

## [0.1.0] - 2026-10-21

Initial public release.

### Added

- `ctxviz statusline`: a category-colored context usage bar for the Cursor CLI status line, with model, workspace, git branch, diff counts, legend, and session duration. Options for a classic layout (`--layout classic`), a labeled bar (`--bar labeled`), and one-row mode (`--rows 1`).
- `ctxviz tui`: an interactive explorer tree, timeline chart, and detail pane that update live as checkpoints land, with a filter, a pager key, and a remembered explorer width.
- `ctxviz snapshot` and `ctxviz watch`: one-shot and streaming reports, as text or JSON (NDJSON for `watch`).
- `ctxviz doctor`: store diagnostics and a field dump of the latest root.
- Kitty integration: `ctxviz open` opens the TUI in a split next to the agent, tab colors and desktop notifications at 80% and 90% usage, a progress indicator, and `ctxviz notify-test`.
- Themes: Modus, Catppuccin, Rosé Pine, Tokyo Night, and Gruvbox presets, `ansi` and `none`, auto light/dark detection, and `--theme-file` for any Kitty color scheme.

### Security

- Chat stores are opened read-only with SQLite defensive mode, `trusted_schema` off, and `query_only` on; `blobs` and `meta` must be real tables, and oversized blobs are skipped.
- The protobuf decoder caps group nesting depth and field counts, so a crafted store cannot overflow the stack.
- Store text is sanitized at every terminal sink (status line, TUI, snapshot and watch output, doctor, pager files): control characters are replaced with U+FFFD.
- Agent and session ids must match `[A-Za-z0-9._-]{1,128}` with no leading dot before they become paths; `--chat` with a bad id is refused, and an explicit store file path is still accepted.
- `git` runs with a cleared environment, global and system config ignored (`GIT_CONFIG_GLOBAL=/dev/null`), hooks, fsmonitor, external diff, textconv, pager, and `core.attributesFile` disabled, every `filter.<driver>` clean and smudge command neutralized, and a 150 ms budget. `--no-git` or `CTXVIZ_NO_GIT=1` turns it off.
- Pager temp files are created with random names and mode 0600 under `<state dir>/pager/`, deleted by the overlay shell after the pager exits, and swept after one hour. The `$PAGER` value is passed with `--env` and never spliced into the shell script.
- State is written only under `~/.cursor/ctxviz` (or `$CTXVIZ_STATE_DIR`, which moves every state file), with directories 0700 and files 0600. Stdin payload, theme file, and state file reads are capped. Caveat: opening a WAL-mode store still makes SQLite create `store.db-wal` and `store.db-shm` next to it.
- No network access and no telemetry.

[Unreleased]: https://github.com/vykluong/ctxviz/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/vykluong/ctxviz/releases/tag/v0.1.0
