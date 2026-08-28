#!/usr/bin/env bash
# Compare the measurement reports written by ./test/measure.sh.
#
#   ./test/measure-compare.sh [reference-tag] [candidate-tag]
#
# Defaults to comparing the Python reference against the Rust build. Reports a
# missing candidate plainly rather than inventing a number for it.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/.out"
REF_TAG="${1:-k8s-sidecar-reference_testing}"
CAND_TAG="${2:-k8s-sidecar-rs_testing}"

field() { sed -n "s/^$2 *: *//p" "$1" | head -1; }

REF="$OUT/measure-$REF_TAG.txt"
CAND="$OUT/measure-$CAND_TAG.txt"

if [ ! -f "$REF" ]; then
  echo "No reference report at $REF -- run: make measure-reference" >&2; exit 2
fi

ratio() { awk -v a="$1" -v b="$2" 'BEGIN{ if (b+0 == 0) print "n/a"; else printf "%.1fx", (a+0)/(b+0) }'; }
mb()    { awk -v k="$1" 'BEGIN{ printf "%.1f", (k+0)/1024 }'; }

r_img=$(field "$REF" image_mb);      r_idle=$(field "$REF" rss_idle_kb); r_load=$(field "$REF" rss_load_kb)

printf '\n\033[1m%-22s %12s %14s %14s\033[0m\n' "implementation" "image MB" "RSS idle MB" "RSS load MB"
printf '%-22s %12s %14s %14s\n' "$(field "$REF" image)" "$r_img" "$(mb "$r_idle")" "$(mb "$r_load")"

if [ ! -f "$CAND" ]; then
  printf '%-22s %12s %14s %14s\n' "$CAND_TAG" "-" "-" "-"
  echo
  echo "No candidate report at $CAND yet (run: make measure-rust once Phase 4 builds)."
  exit 0
fi

c_img=$(field "$CAND" image_mb);     c_idle=$(field "$CAND" rss_idle_kb); c_load=$(field "$CAND" rss_load_kb)
printf '%-22s %12s %14s %14s\n' "$(field "$CAND" image)" "$c_img" "$(mb "$c_idle")" "$(mb "$c_load")"
printf '\n\033[1m%-22s %12s %14s %14s\033[0m\n' "improvement" \
  "$(ratio "$r_img" "$c_img")" "$(ratio "$r_idle" "$c_idle")" "$(ratio "$r_load" "$c_load")"
echo
