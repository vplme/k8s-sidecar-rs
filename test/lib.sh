#!/usr/bin/env bash
# Assertion + wait helpers for the sidecar conformance suite.
#
# Ported from kiwigrid/k8s-sidecar .github/workflows/build_and_test.yaml.
# Difference from upstream CI: assertions tally pass/fail and keep going instead
# of aborting on the first failure, so a run against a candidate implementation
# reports every deviation at once rather than one per iteration.

PASS_COUNT=0
FAIL_COUNT=0
FAILURES=()

_pass() { PASS_COUNT=$((PASS_COUNT + 1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
_fail() {
  FAIL_COUNT=$((FAIL_COUNT + 1)); FAILURES+=("$1")
  printf '  \033[31mFAIL\033[0m %s\n' "$1"
  [ -n "${2:-}" ] && printf '       %s\n' "$2"
  return 0
}

section() { printf '\n\033[1m=== %s ===\033[0m\n' "$1"; }

summary() {
  printf '\n\033[1m=== Summary ===\033[0m\n'
  printf 'passed: %d   failed: %d\n' "$PASS_COUNT" "$FAIL_COUNT"
  if [ "$FAIL_COUNT" -gt 0 ]; then
    printf '\nFailures:\n'
    for f in "${FAILURES[@]}"; do printf '  - %s\n' "$f"; done
    return 1
  fi
  return 0
}

# ---------------------------------------------------------------- assertions

# check_content <expected-string> <local-file>
check_content() {
  local expected="$1" file="$2"
  if [ ! -f "$file" ]; then _fail "content $file" "file does not exist"; return 0; fi
  if echo -n "$expected" | diff -q - "$file" >/dev/null 2>&1; then
    _pass "content $file == '$expected'"
  else
    _fail "content $file == '$expected'" "got: $(head -c 200 "$file" | tr '\n' ' ')"
  fi
}

# check_diff <expected-file> <actual-file>
check_diff() {
  if diff -q "$1" "$2" >/dev/null 2>&1; then _pass "bytes $2 == $1"
  else _fail "bytes $2 == $1" "files differ (or missing)"; fi
}

# check_exists <pod> <path-in-pod>
check_exists() {
  if kubectl exec "$1" -- sh -c "test -e $2" >/dev/null 2>&1; then _pass "exists $1:$2"
  else _fail "exists $1:$2" "missing"; fi
}

# check_not_exists <pod> <path-in-pod>
check_not_exists() {
  if kubectl exec "$1" -- sh -c "! test -e $2" >/dev/null 2>&1; then _pass "absent $1:$2"
  else _fail "absent $1:$2" "still present"; fi
}

# check_log_contains <pattern> <local-log-file>
check_log_contains() {
  if grep -q -- "$1" "$2" 2>/dev/null; then _pass "log $2 contains '$1'"
  else _fail "log $2 contains '$1'" "pattern not found"; fi
}

# check_log_matches <extended-regex> <local-log-file>
check_log_matches() {
  if grep -Eq -- "$1" "$2" 2>/dev/null; then _pass "log $2 matches /$1/"
  else _fail "log $2 matches /$1/" "pattern not found"; fi
}

# check_empty_or_missing <local-file>   (expected outcome for a failed *.url fetch)
check_empty_or_missing() {
  if [ ! -s "$1" ]; then _pass "empty-or-missing $1"
  else _fail "empty-or-missing $1" "size $(wc -c <"$1") bytes"; fi
}

# check_log_count <local-log-file> <pattern> <expected-count>
check_log_count() {
  local count; count=$(grep -c -- "$2" "$1" 2>/dev/null || true)
  if [ "${count:-0}" -eq "$3" ]; then _pass "count $1 '$2' == $3"
  else _fail "count $1 '$2' == $3" "got ${count:-0}"; fi
}

# check_pod_log_count <pod> <path-in-pod> <pattern> <expected-count>
check_pod_log_count() {
  local count; count=$(kubectl exec "$1" -- sh -c "grep -c '$3' $2" 2>/dev/null | tr -d '\r' || true)
  if [ "${count:-0}" -eq "$4" ]; then _pass "count $1:$2 '$3' == $4"
  else _fail "count $1:$2 '$3' == $4" "got ${count:-0}"; fi
}

# check_pod_file_exists <pod> <path-in-pod>
check_pod_file_exists() { check_exists "$1" "$2"; }

# check_http_from_pod <pod> <url> <description>   -- expects success
check_http_from_pod() {
  if kubectl exec "$1" -- python -c \
      "import urllib.request; urllib.request.urlopen('$2', timeout=5)" >/dev/null 2>&1; then
    _pass "$3"
  else _fail "$3" "request to $2 failed"; fi
}

# check_http_from_pod_fails <pod> <url> <description>  -- expects failure
check_http_from_pod_fails() {
  if kubectl exec "$1" -- python -c \
      "import urllib.request; urllib.request.urlopen('$2', timeout=5)" >/dev/null 2>&1; then
    _fail "$3" "request to $2 unexpectedly succeeded"
  else _pass "$3"; fi
}

# ------------------------------------------------------------------- waiting

wait_for_pod_ready() {
  local pod="$1" timeout="${2:-180}"
  echo "waiting for pod $pod (timeout ${timeout}s)..."
  if ! kubectl wait --for=condition=ready --timeout="${timeout}s" "pod/$pod" >/dev/null 2>&1; then
    echo "ERROR: pod $pod not ready within ${timeout}s" >&2
    kubectl describe pod "$pod" >&2 || true
    kubectl logs "$pod" --tail=50 >&2 || true
    return 1
  fi
}

# wait_for_pod_log <pod> <pattern> [since-time]
#
# NOTE: the log is captured into a variable rather than piped into `grep -q`.
# Under `set -o pipefail`, `grep -q` exits as soon as it matches, which sends
# SIGPIPE to `kubectl logs`; the pipeline then reports failure even though the
# pattern WAS found, and the loop can never terminate. It only shows up on pods
# with logs big enough that grep exits before kubectl finishes writing.
wait_for_pod_log() {
  local pod="$1" pattern="$2" since="${3:-}" retries=30 count=0 out
  local args=(); [ -n "$since" ] && args=(--since-time "$since")
  while true; do
    out=$(kubectl logs "$pod" "${args[@]}" 2>/dev/null) || out=""
    if grep -q -- "$pattern" <<<"$out"; then return 0; fi
    count=$((count + 1))
    if [ "$count" -gt "$retries" ]; then
      echo "ERROR: timed out waiting for '$pattern' in logs of pod '$pod'" >&2
      kubectl logs "$pod" --tail=60 >&2 || true
      return 1
    fi
    sleep 5
  done
}
