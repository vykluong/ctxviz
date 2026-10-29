# ctxviz (crate reference)

Live context visualizer for interactive Cursor CLI (`agent`) sessions. Start with
the [top-level README](../README.md) for installation and usage; this file covers
the crate internals. See [`../docs/store-format.md`](../docs/store-format.md)
for the chat store layout.

## Install

From the repo root:

```sh
cargo install --path ctxviz --locked   # installs ~/.cargo/bin/ctxviz
```

For development, `cargo build --release` in `ctxviz/` builds
`./target/release/ctxviz`.

## Status line

Point the Cursor CLI at the binary in `cli-config.json` in its config dir
(`$CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else `~/.cursor`).
`ctxviz doctor` reports the dir ctxviz resolves (`$CTXVIZ_CURSOR_DIR` wins
when set) and confirms the `statusLine` points at this binary:

```json
"statusLine": {
  "command": "/Users/<you>/.cargo/bin/ctxviz statusline",
  "padding": 2,
  "type": "command"
}
```

The command reads the payload JSON on stdin, reads the token breakdown from
the session's chat store and prints two rows. In the default `wide` layout,
row 1 is the model, the parameter badge, a full-width category bar,
`~used/max`, and the session duration; row 2 is the workspace path,
`git:branch`, diff counts, and a right-aligned top-3 legend. The `classic`
layout puts the bar, percent, and legend on row 1 and the model, workspace,
branch, sparkline of recent checkpoints, and last delta on row 2. When the
store or its latest root is missing (a new chat with no checkpoint yet) it
renders from the payload's `used_percentage` and exits 0. On a store error it
exits 1 with empty output, so Cursor keeps the previous text.

Input limits: the stdin payload is capped at 1 MiB (a longer one is
rejected), `render_width_chars` is clamped to 1024, a `--theme-file` is read
only up to 1 MiB, and `watch --interval` has a floor of 0.1 s.

Options:

- `--theme <name>` / `--theme-file <path>`: color theme, as in the
  top-level README's Themes section (env `CTXVIZ_THEME`).
- `--layout wide|classic` (default `wide`): row layout, described above.

- `--bar plain|labeled`: bar style. `plain` (default) draws `█` runs per
  category; `labeled` paints segments as background blocks with the category
  name inside wide ones (needs a renderer that shows background colors).
- `--rows 1|2` (default 2): `1` prints one row with the bar, percent,
  `used/max`, model, `‹effort›`, trend, `Δ`, and legend, and no path or branch.
- `--no-git`: never run `git` or read `.git` in the workspace; the branch
  and diff counts are omitted. `CTXVIZ_NO_GIT` set to anything other than
  empty, `0`, or `false` does the same.

## Subcommands

- `statusline`: render the agent status line from the payload on stdin.
- `tui`: interactive explorer and timeline for a chat (`--split <percent>`,
  `--timeline-rows <rows>`, `--theme`/`--theme-file`).
- `open`: open the TUI in a Kitty split next to the agent window (`--chat`,
  default the store in `current.json`; `--focus`; `--theme`/`--theme-file`,
  forwarded to the TUI).
- `snapshot`: print the context usage report once (`--json`, `--depth N`,
  default 2, `--theme`/`--theme-file`).
- `watch`: stream a snapshot on every checkpoint (`--json` for NDJSON,
  `--depth N`, `--interval` seconds, default 0.5, floor 0.1,
  `--theme`/`--theme-file`). Both color only on a terminal unless a theme is
  passed explicitly.
- `doctor`: check the setup, then diagnose the store layout and dump the
  latest root's field tree (`--depth N`, default 4; `--repeat N` entries per
  repeated field, default 3). The setup lines are `OK:`, `INFO:`, or `WARN:`
  and cover the version and build, the dirs and the four env vars
  (`CTXVIZ_CURSOR_DIR`, `CURSOR_CONFIG_DIR`, `XDG_CONFIG_HOME`,
  `CTXVIZ_STATE_DIR`), the `statusLine` in `<cursor dir>/cli-config.json` and
  in the other candidate dirs, chats, `current.json`, stale temp files, and
  Kitty. Without a store it prints `INFO: no store`. Exits 1 only when a
  `WARN:` line was printed. Read-only.
- `notify-test`: fire a threshold notification and tab color for testing
  (`--percent N`, default 85, `--theme`/`--theme-file`).

`snapshot`, `watch`, `tui` and `doctor` select the chat with `--chat
<agentId|path/to/store.db>`, `--cwd <workspace>` (latest chat there, default
the current directory) or `--latest` (most recently written chat anywhere).
An agent id passed to `--chat` (and a payload `session_id`) must match
`[A-Za-z0-9._-]{1,128}` with no leading dot; a `--chat` path to an existing
file is still opened directly.

## State files

All state lives in one state dir: `$CTXVIZ_STATE_DIR` when set and non-empty,
else `ctxviz/` in the cursor dir (`~/.cursor/ctxviz` by default). The cursor
dir is `$CTXVIZ_CURSOR_DIR`, else `$CURSOR_CONFIG_DIR`, else
`$XDG_CONFIG_HOME/cursor`, else `~/.cursor` (blank counts as unset, relative
is an error); chats are read from `chats/` in it. `CTXVIZ_STATE_DIR` moves
every file below. New directories are created with mode 0700 and files with
mode 0600.

- `<state dir>/current.json`: the session the status line last saw
  (session id, store path, cwd, Kitty window id, model, `cursor_dir` where
  the store was found, and `ctxviz_version`; the last two are absent in older
  files). Used by `open` and the TUI. Written by atomic rename without fsync.
  The status line searches every candidate cursor dir for the session's
  store, so `cursor_dir` can differ from the dir the shell resolves;
  `doctor` warns when it does.
- `<state dir>/tui.json`: TUI layout remembered across runs (the
  explorer width set with `[`/`]`). `--split` overrides it; a missing or
  corrupt file is ignored.
- `<state dir>/open.log`: one line per failed `ctxviz open`, since a
  background launch has no window for stderr. Rotated to `open.log.1` past
  256 KiB.
- `<state dir>/pager/`: short-lived temp files for the TUI's `o` key (mode
  0600, random names), deleted by the Kitty overlay shell after the pager
  exits. Files older than one hour are swept when the TUI starts.
- `<state dir>/<agentId>.json`: timeline cache (checkpoints, scan rowid, last
  notified band); an invalid id maps to `invalid-<md5>.json`. Deleting the
  file resets it; it is rebuilt on the next scan.
- `<state dir>/.<name>.tmp.*`: temp files of an interrupted atomic write.
  `ctxviz tui` removes those older than one hour when it starts.

`ctxviz open` passes `CTXVIZ_CURSOR_DIR` and `CTXVIZ_STATE_DIR` to the Kitty
split with `--env`, since Kitty starts the split with its own environment.

Outside the state dir, opening a WAL-mode chat store read-only still makes
SQLite create `store.db-wal` and `store.db-shm` next to it when they are
missing (see `Store::open` in `src/store.rs`).

What ctxviz reads, writes, and runs, and the fact that nothing leaves the
machine, is summarized in the top-level
[Privacy and security](../README.md#privacy-and-security) section.

## Reference implementation

`../scripts/context_usage.py` is the reference and golden oracle: `ctxviz
snapshot --json` must equal its `--json` output except for the extra
`timeline` key, and `ctxviz watch --json` mirrors `context_usage.py --watch
--json`. `scripts/golden-diff.sh [store.db|agentId|--latest]` runs that
comparison (default: the fixture) and exits non-zero on a diff.

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

## Tests and fixture

`cargo test` runs the decoder unit tests and, when `tests/fixtures/store.db`
exists, a golden comparison against the Python output in
`tests/fixtures/store.json`. Fixtures contain conversation text and are
gitignored. Regenerate them from any chat store:

```sh
scripts/make-fixture.sh ~/.cursor/chats/<md5(cwd)>/<agentId>/store.db
```

## Kitty

`ctxviz open` opens the TUI in a vsplit next to the agent window recorded in
`<state dir>/current.json` (or reports the split already open; `--focus`
focuses it). Suggested `kitty.conf` binding, with the path replaced by the
output of `command -v ctxviz` (see the top-level README for why both the
absolute path and `--allow-remote-control` are needed):

```conf
map cmd+shift+c launch --type=background --allow-remote-control /Users/you/.cargo/bin/ctxviz open
```

`ctxviz notify-test --percent 85` forces a band: tab color via remote control,
plus an OSC 99 notification and OSC 9;4 progress on the tty.

## License

Licensed under either of [Apache License, Version 2.0](../LICENSE-APACHE) or
[MIT license](../LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in the work
by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
