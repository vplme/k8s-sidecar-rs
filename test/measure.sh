#!/usr/bin/env bash
# Phase 3 -- record image size and resident memory for an implementation.
#
#   SIDECAR_IMAGE=k8s-sidecar-reference:testing ./test/measure.sh
#
# Writes test/.out/measure-<tag>.txt and prints a summary. Run once per
# implementation; `make measure-compare` diffs the recorded reports.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/lib.sh"

SIDECAR_IMAGE="${SIDECAR_IMAGE:-k8s-sidecar-reference:testing}"
CLUSTER="${CLUSTER:-sidecar-testing}"
MANIFEST_IMAGE="kiwigrid/k8s-sidecar:testing"
LOAD_COUNT="${LOAD_COUNT:-50}"      # ConfigMaps to sync
LOAD_KB="${LOAD_KB:-8}"             # payload size of each
export KUBE_NAMESPACE=ext-a EXT_NS=ext-a

TAG=$(echo "$SIDECAR_IMAGE" | tr '/:' '__')
# Reports live outside test/.out: run.sh recreates that directory on every
# invocation and would silently erase recorded baselines.
OUT="$HERE/.measure"; mkdir -p "$OUT"
REPORT="$OUT/measure-$TAG.txt"

# rss_kb -- VmRSS of the container's PID 1, in kB.
rss_kb() {
  kubectl exec -n ext-a ext-mem -c sidecar -- \
    sh -c 'grep ^VmRSS: /proc/1/status | tr -s " " | cut -d" " -f2' 2>/dev/null | tr -d '\r'
}

# peak_rss_kb <samples> <interval> -- max of N samples
peak_rss_kb() {
  local n="$1" gap="$2" i max=0 v
  for i in $(seq 1 "$n"); do
    v=$(rss_kb); [ -n "${v:-}" ] && [ "$v" -gt "$max" ] 2>/dev/null && max="$v"
    [ "$i" -lt "$n" ] && sleep "$gap"
  done
  echo "$max"
}

section "Image size"
# `docker image inspect --format {{.Size}}` under-reports badly under the
# containerd image store (28 MB for an image whose layers total ~139 MB), so we
# take the size `docker images` shows -- the number a user would actually see --
# and parse its unit suffix.
IMG_HUMAN=$(docker images "$SIDECAR_IMAGE" --format '{{.Size}}' | head -1)
if [ -z "${IMG_HUMAN:-}" ]; then echo "ERROR: image $SIDECAR_IMAGE not found" >&2; exit 1; fi
IMG_MB=$(awk -v v="$IMG_HUMAN" 'BEGIN{
  n=v+0; u=v; sub(/^[0-9.]+/, "", u);
  if (u ~ /^GB/) n*=1000; else if (u ~ /^kB/) n/=1000; else if (u ~ /^B/) n/=1000000;
  printf "%.1f", n }')
echo "$SIDECAR_IMAGE = $IMG_HUMAN (${IMG_MB} MB)"

section "Deploy measurement pod"
kind load docker-image "$SIDECAR_IMAGE" --name "$CLUSTER" >/dev/null 2>&1
kubectl delete -f "$HERE/ext/pods-mem.yaml" --ignore-not-found --wait --timeout=120s >/dev/null 2>&1
kubectl delete configmap -n ext-a -l extmem --ignore-not-found >/dev/null 2>&1
sed "s|${MANIFEST_IMAGE}|${SIDECAR_IMAGE}|g" "$HERE/ext/pods-mem.yaml" | kubectl apply -f - >/dev/null
wait_for_pod_ready ext-mem 180 || exit 1
echo "settling for 20s..."; sleep 20

section "Idle (no watched resources)"
IDLE_KB=$(peak_rss_kb 4 3)
echo "peak RSS idle: ${IDLE_KB} kB ($(( IDLE_KB / 1024 )) MB)"

section "Under load (${LOAD_COUNT} ConfigMaps x ${LOAD_KB} kB)"
PAYLOAD=$(head -c $(( LOAD_KB * 1024 )) /dev/zero | tr '\0' 'x')
for i in $(seq -w 1 "$LOAD_COUNT"); do
  cat <<EOF | kubectl apply -f - >/dev/null
apiVersion: v1
kind: ConfigMap
metadata: {name: mem-cm-$i, namespace: ext-a, labels: {extmem: "yes"}}
data: {mem-$i.txt: "$PAYLOAD"}
EOF
done
echo "waiting for all ${LOAD_COUNT} files to sync..."
waited=0
until [ "$(kubectl exec -n ext-a ext-mem -c sidecar -- sh -c 'ls /data | wc -l' 2>/dev/null | tr -d '\r')" \
        -ge "$LOAD_COUNT" ] 2>/dev/null || [ "$waited" -ge 180 ]; do
  sleep 5; waited=$((waited + 5))
done
SYNCED=$(kubectl exec -n ext-a ext-mem -c sidecar -- sh -c 'ls /data | wc -l' 2>/dev/null | tr -d '\r')
echo "synced ${SYNCED}/${LOAD_COUNT} files after ${waited}s"
LOAD_KB_RSS=$(peak_rss_kb 6 3)
echo "peak RSS under load: ${LOAD_KB_RSS} kB ($(( LOAD_KB_RSS / 1024 )) MB)"

section "Process inventory (auditability)"
# Recorded so a future implementation that forks children is visible rather
# than silently under-measured by the PID-1 reading above.
PROCS=$(kubectl exec -n ext-a ext-mem -c sidecar -- sh -c \
  'for p in /proc/[0-9]*; do n=$(sed -n "s/^Name:\t//p" $p/status); r=$(sed -n "s/^VmRSS:[ \t]*//p" $p/status); echo "  pid=${p#/proc/} $n ${r:-0}"; done' 2>/dev/null)
echo "$PROCS"

{
  echo "image        : $SIDECAR_IMAGE"
  echo "image_size   : $IMG_HUMAN"
  echo "image_mb     : $IMG_MB"
  echo "rss_idle_kb  : $IDLE_KB"
  echo "rss_load_kb  : $LOAD_KB_RSS"
  echo "load_count   : $LOAD_COUNT"
  echo "load_kb_each : $LOAD_KB"
  echo "synced       : $SYNCED"
  echo "processes    :"
  echo "$PROCS"
} > "$REPORT"

section "Summary"
printf '%-14s %10s %14s %14s\n' image "image MB" "RSS idle MB" "RSS load MB"
printf '%-14s %10s %14s %14s\n' "$TAG" "$IMG_MB" "$(( IDLE_KB / 1024 ))" "$(( LOAD_KB_RSS / 1024 ))"
echo
echo "report written to $REPORT"
