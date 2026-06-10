#!/usr/bin/env bash
# Rust-plugin scale-QA harness (Sprint 3, plan 2026-06-10-rust-plugin-scale-qa.md D2).
#
# Runs an INSTALLED loomweave (venv with the rust plugin discoverable) over one
# corpus and harvests the QA numbers: wall + peak RSS (/usr/bin/time -v — MaxRSS
# covers reaped children via wait4 rusage, i.e. the plugin child), entity/edge
# counts per kind, findings by rule, unresolved-call-site rate, SEI churn on an
# unchanged re-analyze and on a benign one-fn-body edit, and a qualname
# collision sweep. The corpus is treated read-only except the benign-edit
# probe, which is reverted with `git checkout -- .`.
#
# Usage: run_corpus_qa.sh <loomweave-venv-bin> <corpus-dir> <out-dir> [qualname_check-binary]
set -euo pipefail

VBIN="$1"; CORPUS="$2"; OUT="$3"; QCHECK="${4:-}"
LW="$VBIN/loomweave"
NAME="$(basename "$CORPUS")"
mkdir -p "$OUT"

# Hermetic install: never mutate ~/.codex/config.toml (clarion-c5e3cc2818).
export LOOMWEAVE_CODEX_CONFIG="$OUT/codex-config.toml"
# The venv bin dir must be on PATH for plugin discovery (install-prefix chain).
# PATH is RESTRICTED to venv + system dirs so only the venv's plugins are
# discoverable — a user-global loomweave-plugin-python would otherwise be
# picked up and run pyright over any stray .py files in a Rust corpus,
# muddying wall/RSS numbers.
export PATH="$VBIN:/usr/bin:/bin"
export RUST_LOG=info

# Fresh store per run.
rm -rf "$CORPUS/.weft" "$CORPUS/loomweave.yaml"

"$LW" install --path "$CORPUS" >"$OUT/install.log" 2>&1

run_analyze() { # $1 = tag
  /usr/bin/time -v "$LW" analyze "$CORPUS" >"$OUT/analyze-$1.out" 2>"$OUT/analyze-$1.err" || true
  grep -E "Elapsed \(wall clock\)|Maximum resident set size" "$OUT/analyze-$1.err" \
    | sed "s/^[[:space:]]*/$1 /" | tee -a "$OUT/summary.txt"
  grep -o "SEI mint pass complete.*" "$OUT/analyze-$1.err" | tail -1 \
    | sed "s/^/$1 /" | tee -a "$OUT/summary.txt"
  tail -1 "$OUT/analyze-$1.out" | sed "s/^/$1 /" | tee -a "$OUT/summary.txt"
}

echo "== corpus $NAME ($(git -C "$CORPUS" rev-parse HEAD 2>/dev/null || echo unpinned))" | tee "$OUT/summary.txt"
run_analyze first

DB="$CORPUS/.weft/loomweave/loomweave.db"
{
  echo "-- run status / stats"
  sqlite3 "$DB" "SELECT status FROM runs ORDER BY started_at DESC LIMIT 1;"
  sqlite3 "$DB" "SELECT stats FROM runs ORDER BY started_at DESC LIMIT 1;" | python3 -m json.tool || true
  echo "-- entities by kind"
  sqlite3 "$DB" "SELECT kind, COUNT(*) FROM entities GROUP BY kind ORDER BY 2 DESC;"
  echo "-- edges by kind/confidence"
  sqlite3 "$DB" "SELECT kind, confidence, COUNT(*) FROM edges GROUP BY kind, confidence ORDER BY kind;"
  echo "-- findings by rule"
  sqlite3 "$DB" "SELECT rule_id, severity, COUNT(*) FROM findings GROUP BY rule_id, severity;"
  echo "-- guard-tripped / degraded files"
  sqlite3 "$DB" "SELECT f.rule_id, f.message FROM findings f WHERE f.rule_id LIKE 'LMWV-RUST-%' LIMIT 200;"
} >"$OUT/harvest-first.txt" 2>&1

# Unchanged re-analyze: SEI churn MUST be minted=0 orphaned=0.
run_analyze unchanged

# Benign-edit probe: append a no-op line inside the body of one function via a
# comment-free statement; we simply touch a leaf .rs file's fn body with `let _qa = 0;`.
PROBE="$(grep -rl --include='*.rs' -m1 'fn main' "$CORPUS/src" 2>/dev/null | head -1 || true)"
[ -z "$PROBE" ] && PROBE="$(find "$CORPUS" -name '*.rs' -path '*/src/*' | head -1)"
if [ -n "$PROBE" ] && git -C "$CORPUS" rev-parse >/dev/null 2>&1; then
  python3 - "$PROBE" <<'EOF'
import re, sys
p = sys.argv[1]
s = open(p).read()
m = re.search(r'fn [a-z_0-9]+\s*\([^)]*\)[^{;]*\{', s)
if m:
    i = m.end()
    open(p, 'w').write(s[:i] + ' let _qa_probe = 0; let _ = _qa_probe; ' + s[i:])
    print(f"probe edited: {p}")
else:
    print(f"probe SKIPPED (no fn body found): {p}")
EOF
  run_analyze benign-edit
  git -C "$CORPUS" checkout -- . 2>/dev/null || true
fi

# Qualname collision sweep (example binary built from the worktree).
if [ -n "$QCHECK" ] && [ -x "$QCHECK" ]; then
  "$QCHECK" "$CORPUS" | tee -a "$OUT/summary.txt" || echo "QUALNAME COLLISIONS in $NAME" | tee -a "$OUT/summary.txt"
fi

echo "== $NAME done; harvest in $OUT" | tee -a "$OUT/summary.txt"
