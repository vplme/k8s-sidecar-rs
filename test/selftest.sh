#!/usr/bin/env bash
# Negative controls for the extended suite's assertions.
#
# A conformance suite that cannot go red is worthless as an oracle: if an
# assertion silently no-ops (wrong container, wrong path, a helper that swallows
# its own error) it will pass against any implementation, including a broken
# one. Every assertion type below is deliberately given a false claim and MUST
# report a failure.
#
# Run after ./test/run-ext.sh, which creates the pods this inspects.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# Set before sourcing: lib.sh applies its own defaults to these, so exporting
# afterwards would leave the (much slower) 45s propagation wait in place. The
# negative controls assert things that are already settled, so they need no
# propagation window.
export EXT_NS="${EXT_NS:-ext-a}"
export EXT_WAIT="${EXT_WAIT:-6}"
source "$HERE/lib.sh"

for p in ext-mode ext-gate ext-ignore; do
  if ! kubectl get pod -n "$EXT_NS" "$p" >/dev/null 2>&1; then
    echo "ERROR: pod $p not found in $EXT_NS -- run ./test/run-ext.sh first" >&2
    exit 2
  fi
done

section "Negative controls -- every one of these MUST fail"
ext_content      ext-mode /data/m.txt WRONG                  "NC1 wrong content"
ext_mode         ext-mode /data/m.txt 777                    "NC2 wrong mode"
ext_absent       ext-mode /data/m.txt                        "NC3 false absence"
ext_exists       ext-mode /data/no-such-file                 "NC4 missing file"
ext_count        ext-gate /out/script.log SCRIPT_RAN 99      "NC5 wrong count"
ext_log_contains ext-ignore "STRING-THAT-IS-NEVER-LOGGED"    "NC6 missing log line"

EXPECTED=6
echo
if [ "$FAIL_COUNT" -eq "$EXPECTED" ] && [ "$PASS_COUNT" -eq 0 ]; then
  printf '\033[32mSELFTEST OK\033[0m -- all %d negative controls failed as required\n' "$EXPECTED"
  exit 0
fi
printf '\033[31mSELFTEST BROKEN\033[0m -- expected %d failures and 0 passes, got %d failures and %d passes.\n' \
  "$EXPECTED" "$FAIL_COUNT" "$PASS_COUNT"
echo "An assertion that cannot fail will pass against a broken implementation."
exit 1
