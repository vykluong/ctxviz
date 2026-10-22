#!/bin/bash
# Plan verification item 2: diff `ctxviz snapshot --json` (minus `timeline`)
# against the Python reference `context_usage.py --json`.
# Usage: scripts/golden-diff.sh [store.db | agentId | --latest]
#   default: tests/fixtures/store.db. Binary: $CTXVIZ, else target/release/ctxviz.
# Live stores can gain a checkpoint mid-run, so Python runs before and after
# Rust and the Rust output must match one of the two. Exits 1 on a diff.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
bin="${CTXVIZ:-$here/target/release/ctxviz}"
arg="${1:-$here/tests/fixtures/store.db}"
if [ "$arg" = "--latest" ]; then sel=(--latest); else sel=(--chat "$arg"); fi
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
py() { python3 -I "$here/../scripts/context_usage.py" "${sel[@]}" --json | jq -S . > "$1"; }
py "$tmp/py1.json"
"$bin" snapshot "${sel[@]}" --json | jq -S 'del(.timeline)' > "$tmp/rs.json"
py "$tmp/py2.json"
if cmp -s "$tmp/rs.json" "$tmp/py1.json" || cmp -s "$tmp/rs.json" "$tmp/py2.json"; then
  echo "golden-diff: OK (${sel[*]})"
else
  diff "$tmp/rs.json" "$tmp/py2.json" | head -40 >&2 || true
  echo "golden-diff: MISMATCH (${sel[*]})" >&2
  exit 1
fi
