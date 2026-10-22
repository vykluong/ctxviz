#!/bin/sh
# Regenerate tests/fixtures/{store.db,store.json} from a live chat store.
# Usage: scripts/make-fixture.sh <path/to/store.db>
# The fixture holds conversation text: it is gitignored, keep it local.
set -eu
src=${1:?usage: make-fixture.sh <store.db>}
# The source is opened as a SQLite URI (file:...?mode=ro), where `?` starts
# the query string and `#` a fragment, so a path containing them would be
# misread. `%` is a URI escape, and `'` would break the quoting below.
case $src in
  *'?'* | *'#'* | *'%'* | *"'"*)
    echo "make-fixture: refusing store path containing ?, #, % or ': $src" >&2
    exit 2
    ;;
esac
[ -f "$src" ] || { echo "make-fixture: not a file: $src" >&2; exit 2; }
here=$(cd "$(dirname "$0")/.." && pwd)
out="$here/tests/fixtures"
case $out in
  *"'"*) echo "make-fixture: refusing output dir containing ': $out" >&2; exit 2 ;;
esac
cat >&2 <<'BANNER'
########################################################################
#  WARNING: the fixture is a full copy of a real chat store.           #
#  tests/fixtures/store.db and store.json contain PRIVATE conversation #
#  text (prompts, tool output, file contents, possibly secrets).       #
#  NEVER commit them. tests/fixtures/ is gitignored; keep it that way. #
########################################################################
BANNER
umask 077
mkdir -p "$out"
rm -f "$out/store.db" "$out/store.db-wal" "$out/store.db-shm"
# The dot-command argument is single-quoted so sqlite3 does not split it.
sqlite3 "file:$src?mode=ro" ".backup '$out/store.db'"
cd "$here"
python3 -I "$here/../scripts/context_usage.py" --chat tests/fixtures/store.db --json > "$out/store.json"
if git -C "$here" ls-files --error-unmatch tests/fixtures/store.db tests/fixtures/store.json >/dev/null 2>&1; then
  echo "make-fixture: WARNING: fixture files are tracked by git; remove them from the index" >&2
fi
if ! git -C "$here" check-ignore -q tests/fixtures/store.db 2>/dev/null; then
  echo "make-fixture: WARNING: tests/fixtures/store.db is not gitignored" >&2
fi
echo "wrote $out/store.db and $out/store.json (private: do not commit)"
