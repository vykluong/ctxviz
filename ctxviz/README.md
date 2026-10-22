# ctxviz (crate reference)

Live context visualizer for interactive Cursor CLI (`agent`) sessions. Start with the [top-level README](../README.md) for installation, screenshots, and usage; this file covers the crate internals. See [`../docs/store-format.md`](../docs/store-format.md) for the chat store layout.

## Contents

- [Install](#install)
- [Subcommands](#subcommands)
- [Status line](#status-line)
- [Source layout](#source-layout)
- [State files](#state-files)
- [Kitty](#kitty)
- [Reference implementation](#reference-implementation)
- [Tests and fixture](#tests-and-fixture)
- [Verification](#verification)
- [License](#license)

## Install

From the repo root:

```sh
cargo install --path ctxviz --locked   # installs ~/.cargo/bin/ctxviz
```

For development, `cargo build --release` in `ctxviz/` builds `./target/release/ctxviz`.

## Subcommands

| Subcommand | What it does | Options |
| --- | --- | --- |
| `statusline` | render the agent status line from the payload on stdin | see [Status line](#status-line) |
| `tui` | interactive explorer, timeline, and detail pane for a chat | `--split <percent>`, `--timeline-rows <rows>`, theme flags |
| `open` | open the TUI in a Kitty split next to the agent window | `--chat` (default: the store in `current.json`), `--focus`, theme flags (forwarded to the TUI) |
| `snapshot` | print the context usage report once | `--json`, `--depth N` (default 2), theme flags |
| `watch` | stream a snapshot on every checkpoint | `--json` (NDJSON), `--depth N`, `--interval` seconds (default 0.5, floor 0.1), theme flags |
| `doctor` | diagnose the store layout and dump the latest root's field tree | `--depth N` (default 4), `--repeat N` entries per repeated field (default 3) |
| `notify-test` | fire a threshold notification and tab color for testing | `--percent N` (default 85), theme flags |

The theme flags are `--theme <name>` and `--theme-file <path>`, as in the top-level README's [Themes](../README.md#themes) section (env `CTXVIZ_THEME`). `snapshot` and `watch` color only on a terminal unless a theme is passed explicitly.

`snapshot`, `watch`, `tui`, and `doctor` select the chat with `--chat <agentId|path/to/store.db>`, `--cwd <workspace>` (latest chat there, default the current directory), or `--latest` (most recently written chat anywhere). An agent id passed to `--chat` (and a payload `session_id`) must match `[A-Za-z0-9._-]{1,128}` with no leading dot; a `--chat` path to an existing file is still opened directly.

## Status line

Point the Cursor CLI at the binary in `~/.cursor/cli-config.json`:

```json
"statusLine": {
  "type": "command",
  "command": "/Users/<you>/.cargo/bin/ctxviz statusline",
  "padding": 2
}
```

The command reads the payload JSON on stdin, reads the token breakdown from the session's chat store, and prints two rows:

- **`wide`** (default): row 1 is the model, the parameter badge, a full-width category bar, `~used/max`, and the session duration; row 2 is the workspace path, `git:branch`, diff counts, and a right-aligned top-3 legend.
- **`classic`**: row 1 is the bar, percent, and legend; row 2 is the model, workspace, branch, a sparkline of recent checkpoints, and the last delta.

When the store or its latest root is missing (a new chat with no checkpoint yet), it renders from the payload's `used_percentage` and exits 0. On a store error it exits 1 with empty output, so Cursor keeps the previous text.

Options:

- `--layout wide|classic` (default `wide`): row layout, described above.
- `--bar plain|labeled` (default `plain`): `plain` draws `█` runs per category; `labeled` paints segments as background blocks with the category name inside wide ones (needs a renderer that shows background colors).
- `--rows 1|2` (default 2): `1` prints one row with the bar, percent, `used/max`, model, `‹effort›`, trend, `Δ`, and legend, and no path or branch.
- `--no-git`: never run `git` or read `.git` in the workspace; the branch and diff counts are omitted. `CTXVIZ_NO_GIT` set to anything other than empty, `0`, or `false` does the same.
- `--theme <name>` / `--theme-file <path>`: color theme.

Input limits: the stdin payload is capped at 1 MiB (a longer one is rejected), `render_width_chars` is clamped to 1024, a `--theme-file` is read only up to 1 MiB, and `watch --interval` has a floor of 0.1 s.

## Source layout

| Path | Contents |
| --- | --- |
| `src/main.rs` | CLI entry point: parses arguments and dispatches to a subcommand |
| `src/commands/` | one module per subcommand, each with its clap `Args` |
| `src/store.rs` | read-only access to a chat store (SQLite, defensive mode) |
| `src/pb.rs` | schema-less protobuf wire decoder (port of `scripts/pb.py`) |
| `src/schema.rs` | every protobuf field number ctxviz relies on, in one place |
| `src/model/` | the token breakdown, category table, explorer tree, timeline cache, and the `Snapshot` the renderers share |
| `src/render/` | stacked bar, status line (`wide` and `classic`), and snapshot JSON |
| `src/tui/` | ratatui app state, key handling, the store watcher thread, and one module per panel in `panels/` |
| `src/theme.rs` | theme presets, semantic color roles, and detection |
| `src/kitty.rs` | Kitty remote control, OSC 99 notifications, and OSC 9;4 progress |
| `src/session.rs` | the state dir, `current.json`, and `tui.json` |
| `src/text.rs` | sanitizing untrusted text before it reaches a terminal |
| `tests/` | integration tests: the fixture golden test, sanitizing, store hardening, and the timeline |
| `scripts/` | `golden-diff.sh` and `make-fixture.sh` |

If Cursor renumbers protobuf fields, `src/schema.rs` is the only file that should need to change; `ctxviz doctor` prints the field tree to compare against.

## State files

All state lives in one state dir: `$CTXVIZ_STATE_DIR` when set and non-empty, else `~/.cursor/ctxviz`. The variable moves every file below. New directories are created with mode 0700 and files with mode 0600.

| Path in the state dir | Purpose |
| --- | --- |
| `current.json` | the session the status line last saw (session id, store path, cwd, Kitty window id, model); used by `open` and the TUI |
| `tui.json` | TUI layout remembered across runs (the explorer width set with `[` and `]`); `--split` overrides it, and a missing or corrupt file is ignored |
| `open.log` | one line per failed `ctxviz open`, since a background launch has no window for stderr; rotated to `open.log.1` past 256 KiB |
| `pager/` | short-lived temp files for the TUI's `o` key (mode 0600, random names), deleted by the Kitty overlay shell after the pager exits; files older than one hour are swept when the TUI starts |
| `<agentId>.json` | timeline cache (checkpoints, scan rowid, last notified band); an invalid id maps to `invalid-<md5>.json`; deleting the file resets it, and it is rebuilt on the next scan |

Outside the state dir, opening a WAL-mode chat store read-only still makes SQLite create `store.db-wal` and `store.db-shm` next to it when they are missing (see `Store::open` in `src/store.rs`).

What ctxviz reads, writes, and runs, and the fact that nothing leaves the machine, is summarized in the top-level [Privacy and security](../README.md#privacy-and-security) section.

## Kitty

`ctxviz open` opens the TUI in a vsplit next to the agent window recorded in `current.json` (or reports the split already open; `--focus` focuses it). Suggested `kitty.conf` binding, with the path replaced by the output of `command -v ctxviz` (see the [top-level README](../README.md#4-kitty-one-key-to-open-the-explorer-next-to-the-agent) for why both the absolute path and `--allow-remote-control` are needed):

```conf
map cmd+shift+c launch --type=background --allow-remote-control /Users/<you>/.cargo/bin/ctxviz open
```

`ctxviz notify-test --percent 85` forces a band: tab color via remote control, plus an OSC 99 notification and OSC 9;4 progress on the tty.

## Reference implementation

`../scripts/context_usage.py` is the reference and golden oracle: `ctxviz snapshot --json` must equal its `--json` output except for the extra `timeline` key, and `ctxviz watch --json` mirrors `context_usage.py --watch --json`. `scripts/golden-diff.sh [store.db|agentId|--latest]` runs that comparison (default: the fixture) and exits non-zero on a diff.

## Tests and fixture

`cargo test` runs the decoder unit tests and, when `tests/fixtures/store.db` exists, a golden comparison against the Python output in `tests/fixtures/store.json`. Fixtures contain conversation text and are gitignored. Regenerate them from any chat store:

```sh
scripts/make-fixture.sh ~/.cursor/chats/<md5(cwd)>/<agentId>/store.db
```

## Verification

```sh
# 1. unit tests and the fixture golden test
cargo test

# 2. golden diff against the Python oracle
python3 -I ../scripts/context_usage.py --latest --json > /tmp/py.json
ctxviz snapshot --latest --json > /tmp/rs.json
jq -S 'del(.timeline)' /tmp/rs.json | diff - <(jq -S . /tmp/py.json)

# 3. status line from a sample payload, timed
time (echo '{"session_id":"<id>","cwd":"<ws>","model":{"display_name":"Opus"},"context_window":{"used_percentage":25,"context_window_size":200000},"render_width_chars":120}' | ctxviz statusline)
```

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or [MIT license](../LICENSE-MIT) at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
