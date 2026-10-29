# Contributing to ctxviz

Thanks for your interest. Bug reports, fixes, and small focused features are welcome. For anything larger, open an issue first so we can agree on the approach.

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report security issues privately as described in [SECURITY.md](SECURITY.md), not in a public issue.

## Build

The crate lives in `ctxviz/`:

```sh
cd ctxviz
cargo build
```

## Checks

Run all of these before opening a pull request. CI runs the same set.

```sh
cd ctxviz
cargo test --all-targets --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo fmt --check
cargo deny check
```

`cargo deny` is installed with `cargo install cargo-deny --locked`.

## Fixtures: never commit or share them

> **Warning.** `ctxviz/tests/fixtures/` holds copies of real Cursor chat stores. They contain your conversation text, tool output, and the contents of files the agent read, which can include source code, credentials, and other secrets.
>
> The directory is gitignored. Never commit it, never force-add it, and never attach a fixture to an issue or pull request. `scripts/make-fixture.sh` prints the same warning when it runs.

To create a local fixture from one of your own stores:

```sh
cd ctxviz
scripts/make-fixture.sh ~/.cursor/chats/<md5(cwd)>/<agentId>/store.db   # or under your CTXVIZ_CURSOR_DIR
```

The golden test is skipped when no fixture exists, so `cargo test` passes on a fresh clone. If a bug only reproduces with a particular store, describe its shape (for example the `ctxviz doctor` field dump with text removed) instead of sharing the file.

## Commit style

Commits follow [Conventional Commits](https://www.conventionalcommits.org/): a type, an optional scope, and a short imperative summary. Examples from the history:

```text
feat(statusline): --rows 1 one-row mode
fix(tui): ellipsize the timeline summary; correct README keys
docs: list open.log among the files ctxviz writes
refactor(statusline): plumb RenderOpts (--rows, --bar) with no output change
chore: privacy scrub of docs and sources; add text sanitizer module
```

Common types are `feat`, `fix`, `docs`, `refactor`, `test`, and `chore`. Common scopes are `statusline`, `tui`, and `open`.

## Versioning and releases

ctxviz follows [Semantic Versioning](https://semver.org/). Before 1.0, new functionality bumps the minor version and fixes bump the patch version.

- **Pre-releases on branches.** A branch that changes behavior sets a pre-release version, `X.Y.Z-rc.N` (for example `0.2.0-rc.1`), and increments `N` for each later candidate.
- **One commit per version change.** Bump `version` in `ctxviz/Cargo.toml`, commit the updated `Cargo.lock`, and add the `## [X.Y.Z-rc.N] - YYYY-MM-DD` heading to `CHANGELOG.md`, all in the same commit.
- **GA on `main`.** After the branch merges, a `chore(release): X.Y.Z` commit on `main` drops the pre-release suffix and dates the `## [X.Y.Z]` heading. Tag that commit `vX.Y.Z`.

CI enforces these rules:

- `cargo clippy` and `cargo test` run with `--locked`, so a `Cargo.lock` that disagrees with `Cargo.toml` fails the build.
- A version job requires a `## [<version>]` heading in `CHANGELOG.md` for the version in `Cargo.toml`.
- On a tag push, the tag without its leading `v` must equal that version.

`ctxviz --version` prints the version and the commit it was built from, for example `ctxviz 0.2.0-rc.1 (975a453abc)`, with `-dirty` when tracked files had uncommitted changes and `(unknown)` when built without git.

## Pull requests

- Keep each pull request to one change, and update `README.md` and `CHANGELOG.md` when behavior changes.
- CI must be green before a pull request is merged.
- Do not add network access or telemetry. ctxviz is local-only by design (see [SECURITY.md](SECURITY.md)).

## License

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as below, without any additional terms or conditions.

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
