# Security policy

## Reporting a vulnerability

Please report security issues privately through GitHub security advisories:

<https://github.com/vykluong/ctxviz/security/advisories/new>

Do not open a public issue for a vulnerability. Include the ctxviz version (`ctxviz --version`), your OS and terminal, and steps to reproduce. You should get a first response within a week. Once a fix is released, the advisory is published with credit to the reporter unless you ask otherwise.

## Supported versions

ctxviz is pre-1.0. Only the latest 0.x release receives security fixes.

| Version | Supported |
| --- | --- |
| latest 0.x | yes |
| older | no |

## Threat model

ctxviz is a local, single-user tool. It runs with your privileges and treats you, your shell configuration, and your terminal as trusted. The inputs it treats as untrusted are the contents of Cursor CLI chat stores (conversation text, tool output, file contents the agent read) and the status line payload on stdin.

**What it reads.**

- Chat stores at `~/.cursor/chats/<md5(cwd)>/<agentId>/store.db`, opened read-only with `SQLITE_DBCONFIG_DEFENSIVE`, `trusted_schema=OFF`, and `query_only=1`. `blobs` and `meta` must be real tables, not views. Blobs over 64 MiB are skipped, and the protobuf decoder caps group nesting at 32 levels and the field count per message.
- The status line payload JSON on stdin, capped at 1 MiB, with `render_width_chars` clamped to 1024.
- Its own state files (listed below), each capped at 1 MiB; a larger file reads as missing.
- `.git/HEAD` in the workspace for the branch name, capped at 4 KiB.
- A Kitty theme file when you pass `--theme-file`, capped at 1 MiB.

**Ids become paths only after validation.** Agent ids from `--chat`, the payload's `session_id`, and the session id in `current.json` must match `[A-Za-z0-9._-]{1,128}` with no leading dot, so they cannot contain path separators or `..`. Bad ids are refused. An explicit `--chat` path to an existing file is still opened directly. Agent ids read from a store that fail the rule get a hashed timeline cache name.

**What it writes.** Only under `~/.cursor/ctxviz`, or `$CTXVIZ_STATE_DIR` when set and non-empty (the variable moves every file below). New directories get mode 0700 and files mode 0600:

- `current.json`: the session the status line last saw.
- `tui.json`: the remembered TUI layout.
- `open.log`: one line per failed `ctxviz open`, rotated to `open.log.1` past 256 KiB.
- `<agentId>.json`: per-chat timeline caches.
- `pager/`: temp files for the `o` (pager) key. The directory is kept at mode 0700 and a symlink there is refused. Each file gets a random name, mode 0600, and `create_new`, so an existing file or planted symlink is never followed. It holds sanitized text and is deleted by the Kitty overlay shell after the pager exits, or right away when the launch fails. Files older than one hour are swept when the TUI starts.

**SQLite sidecars.** ctxviz never modifies a chat store or the workspace. One exception to "only under the state dir" is unavoidable: opening a WAL-mode store, even read-only, makes SQLite create `store.db-wal` (empty) and `store.db-shm` next to the store in `~/.cursor/chats/...` when they are missing, and they remain after ctxviz exits. A reader needs the WAL index, so read-only mode cannot prevent this. The Cursor CLI leaves the same files while a chat is live.

**What it executes.**

- `git` inside the workspace, for the diff counts (the branch name is read from `.git/HEAD` directly). The workspace and its `.git/config` are untrusted, so git runs with:
  - a cleared environment that keeps only `PATH` and `HOME`, plus `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1`, so global and system config, including user aliases, are ignored;
  - `-c` overrides for `core.fsmonitor=false`, `core.hooksPath=/dev/null`, `diff.external=`, `core.pager=cat`, and `core.attributesFile=/dev/null`, plus `--no-pager`, `--no-ext-diff`, and `--no-textconv`;
  - every `filter.<driver>` found in the repository config neutralized with empty `clean`, `smudge`, and `process` commands and `required=false`. If a driver name cannot be passed safely as `-c key=value`, git is skipped;
  - a 150 ms budget shared by the config lookup and the diff, after which git is killed.

  Disable it with `--no-git` or `CTXVIZ_NO_GIT` (any value but empty, `0`, or `false`), which also stops ctxviz from reading `.git`.
- `kitten @`, only inside Kitty, over the socket named in `KITTY_LISTEN_ON`, to find windows, open the split, and color tabs.
- Your `$PAGER` (default `less`), as seen by the TUI, when you press `o`. The TUI starts a Kitty overlay running a fixed `sh -c` script and passes the pager command to it with `kitten @ launch --env`, so the value is never spliced into the script text. The script expands it unquoted with globbing off: `less -R` splits into a command and its flags, but no shell syntax in it is evaluated.

**What leaves the machine.** Nothing. ctxviz makes no network connections and has no telemetry.

**Terminal output.** Text from the store is sanitized before it reaches any terminal: control characters are replaced with U+FFFD, so a chat cannot inject escape sequences into the status line, the TUI, the snapshot output, or the pager.

## Accepted risks

These are known and intentionally out of scope:

- **`PATH` resolution.** `kitten`, `git`, and `sh` are resolved through `PATH`. Anyone who can change your `PATH` can already run code as you, so ctxviz extends them the same trust as your shell.
- **`current.json` is trusted state.** It is written with mode 0600 in a directory you own and is read back without further validation of its origin. Another process running as you can edit it, but that process already has your privileges.
- **Notification text.** The OSC 99 desktop notification includes the chat title, which comes from the store. Control characters are stripped, but a chat can still choose a misleading title. The effect is cosmetic.
- **Python reference implementation.** `scripts/context_usage.py` is a developer tool used as the golden-test oracle. It is not a user-facing surface and does not get the same hardening as the binary.
