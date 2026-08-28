#!/usr/bin/env bash
# Conformance suite for k8s-sidecar implementations.
#
# Runs the upstream kiwigrid/k8s-sidecar integration suite against whichever
# image SIDECAR_IMAGE names, so the Python reference and the Rust rewrite are
# held to exactly the same contract.
#
#   SIDECAR_IMAGE=k8s-sidecar-reference:testing ./test/run.sh
#   SIDECAR_IMAGE=k8s-sidecar-rs:testing        ./test/run.sh
#
# Assumes a kind cluster created by ./test/cluster.sh is the current context.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=test/lib.sh
source "$HERE/lib.sh"

SIDECAR_IMAGE="${SIDECAR_IMAGE:-k8s-sidecar-reference:testing}"
DUMMY_IMAGE="${DUMMY_IMAGE:-dummy-server:1.0.0}"
CLUSTER="${CLUSTER:-sidecar-testing}"
# The image tag hard-coded in the upstream manifests, which we substitute.
MANIFEST_IMAGE="kiwigrid/k8s-sidecar:testing"

OUT="${OUT:-$HERE/.out}"
LOGS="$OUT/logs"
rm -rf "$OUT"
mkdir -p "$LOGS" "$OUT/sidecar" "$OUT/sidecar-5xx" \
         "$OUT/sidecar-basicauth-args" "$OUT/sidecar-basicauth-envfile"

echo "Image under test : $SIDECAR_IMAGE"
echo "Cluster          : $CLUSTER"
echo "Artifacts        : $OUT"

# All sidecar pods defined in test/resources/sidecar.yaml.
SIDECAR_PODS=(
  sidecar sidecar-healthcheck sidecar-healthcheck-ipv4
  sidecar-basicauth-args sidecar-basicauth-envfile sidecar-5xx
  sidecar-pythonscript sidecar-pythonscript-logfile
  sidecar-pythonscript-resource-name sidecar-logtofile-pythonscript
  sidecar-sleep
)

# ---------------------------------------------------------------------------
section "Load images into kind"
kind load docker-image "$SIDECAR_IMAGE" --name "$CLUSTER" || exit 1
kind load docker-image "$DUMMY_IMAGE"   --name "$CLUSTER" || exit 1

# ---------------------------------------------------------------------------
section "Install sidecars and dummy server"
# Start from a clean slate so repeated runs against different images don't
# inherit files written by the previous implementation.
kubectl delete -f "$HERE/resources/sidecar.yaml" --ignore-not-found \
        --wait --timeout=120s >/dev/null 2>&1
# The watched resources MUST NOT survive into the next run: if they already
# exist when the sidecar pods start, the initial LIST sync absorbs them all in
# one batch (one SCRIPT invocation) instead of arriving as individual watch
# events, and the script-execution counts below come out far too low.
kubectl delete -f "$HERE/resources/resources.yaml" --ignore-not-found \
        --wait --timeout=120s >/dev/null 2>&1
kubectl delete configmap,secret -l findme --ignore-not-found >/dev/null 2>&1

# SCRIPT_FLAVOR=sh rewrites script-configmap's script.py from Python to sh.
# Four pods run it via SCRIPT; with a non-Python sidecar image the
# `#!/usr/bin/env python` shebang cannot execute. The replacement emits the
# exact same marker line the log-count assertions grep for, so the assertions
# stay unchanged. The vendored manifest itself is never edited.
SCRIPT_SUBST=(-e "s|${MANIFEST_IMAGE}|${SIDECAR_IMAGE}|g")
if [ "${SCRIPT_FLAVOR:-python}" = "sh" ]; then
  SCRIPT_SUBST+=(-e 's|#!/usr/bin/env python|#!/bin/sh|' \
                 -e 's|print("Hello from python script!")|echo "Hello from python script!"|')
fi
sed "${SCRIPT_SUBST[@]}" "$HERE/resources/sidecar.yaml" \
  | kubectl apply -f - || exit 1

for pod in "${SIDECAR_PODS[@]}" dummy-server-pod; do
  wait_for_pod_ready "$pod" || exit 1
done
wait_for_pod_log sidecar-healthcheck      "Starting health server on port 8888" || exit 1
wait_for_pod_log sidecar-healthcheck-ipv4 "Starting health server on port 8888" || exit 1

# ---------------------------------------------------------------------------
section "Install ConfigMaps and Secrets"
# Pods report ready before every watch subprocess is actually streaming.
echo "settling for 20s before applying resources..."
sleep 20
kubectl apply -f "$HERE/resources/resources.yaml" || exit 1

WATCHING_PODS=(sidecar sidecar-basicauth-args sidecar-basicauth-envfile
               sidecar-5xx sidecar-pythonscript sidecar-pythonscript-logfile)
RESOURCES=(sample-configmap sample-secret-binary absolute-configmap
           relative-configmap change-dir-configmap similar-configmap-secret
           url-configmap-500 url-configmap-basic-auth)
for p in "${WATCHING_PODS[@]}"; do
  for r in "${RESOURCES[@]}"; do wait_for_pod_log "$p" "$r" || exit 1; done
done
# This pod monitors only named resources.
for r in sample-configmap sample-secret-binary; do
  wait_for_pod_log sidecar-pythonscript-resource-name "$r" || exit 1
done
echo "settling for 10s after last log line..."
sleep 10

# ---------------------------------------------------------------------------
section "Verify Kubernetes config loading"
K8S_API_IP=$(kubectl get svc kubernetes -o jsonpath='{.spec.clusterIP}')
echo "kubernetes API ClusterIP: $K8S_API_IP"
wait_for_pod_log sidecar       "Config for cluster api at 'https://$K8S_API_IP:443'" || true
wait_for_pod_log sidecar-sleep "Config for cluster api at 'https://$K8S_API_IP:443'" || true

# ---------------------------------------------------------------------------
section "Capture pod logs (pre-update)"
# Captured BEFORE change_resources.yaml is applied: the python-script log counts
# below are defined against the initial sync only.
for p in "${SIDECAR_PODS[@]}" dummy-server-pod; do
  kubectl logs "$p" > "$LOGS/$p.log" 2>&1 || true
done
ls -la "$LOGS"

# ---------------------------------------------------------------------------
section "Download files written by the sidecars"
cp_out() { kubectl cp "$1" "$2" >/dev/null 2>&1 || true; }
for f in hello.world cm-kubelogo.png secret-kubelogo.png \
         url-downloaded-kubelogo.png script_result 500.txt secured.txt \
         similar-configmap.txt similar-secret.txt; do
  cp_out "sidecar:/tmp/$f" "$OUT/sidecar/$f"
done
cp_out sidecar:/tmp/absolute/absolute.txt   "$OUT/sidecar/absolute.txt"
cp_out sidecar:/tmp/relative/relative.txt   "$OUT/sidecar/relative.txt"
cp_out sidecar:/tmp/orig-dir/change-dir.txt "$OUT/sidecar/change-dir.txt"

cp_out sidecar-basicauth-args:/tmp/secured.txt    "$OUT/sidecar-basicauth-args/secured.txt"
cp_out sidecar-basicauth-envfile:/tmp/secured.txt "$OUT/sidecar-basicauth-envfile/secured.txt"

for f in hello.world cm-kubelogo.png secret-kubelogo.png \
         url-downloaded-kubelogo.png 500.txt secured.txt \
         similar-configmap.txt similar-secret.txt; do
  cp_out "sidecar-5xx:/tmp-5xx/$f" "$OUT/sidecar-5xx/$f"
done
cp_out sidecar-5xx:/tmp/script_result             "$OUT/sidecar-5xx/script_result"
cp_out sidecar-5xx:/tmp/absolute/absolute.txt     "$OUT/sidecar-5xx/absolute.txt"
cp_out sidecar-5xx:/tmp-5xx/relative/relative.txt "$OUT/sidecar-5xx/relative.txt"
cp_out sidecar-5xx:/tmp-5xx/orig-dir/change-dir.txt "$OUT/sidecar-5xx/change-dir.txt"

# ---------------------------------------------------------------------------
section "Verify sidecar files after initial sync"
check_content "Hello World!"           "$OUT/sidecar/hello.world"
check_diff    "$HERE/kubelogo.png"     "$OUT/sidecar/cm-kubelogo.png"
check_diff    "$HERE/kubelogo.png"     "$OUT/sidecar/secret-kubelogo.png"
check_diff    "$HERE/kubelogo.png"     "$OUT/sidecar/url-downloaded-kubelogo.png"
check_content "This absolutely exists" "$OUT/sidecar/absolute.txt"
check_content "This relatively exists" "$OUT/sidecar/relative.txt"
check_content "This change-dir exists" "$OUT/sidecar/change-dir.txt"
check_content "I'm very similar"       "$OUT/sidecar/similar-configmap.txt"
check_content "I'm very similar"       "$OUT/sidecar/similar-secret.txt"
check_content "allowed"                "$OUT/sidecar/secured.txt"
check_empty_or_missing                 "$OUT/sidecar/500.txt"
check_exists  sidecar /tmp/script_result

# ---------------------------------------------------------------------------
section "Verify health server"
check_log_contains "Starting health server on port 8888" "$LOGS/sidecar-healthcheck.log"
check_http_from_pod sidecar-healthcheck "http://0.0.0.0:8888/healthz" \
  "health server answers on IPv4"
check_http_from_pod sidecar-healthcheck "http://[::1]:8888/healthz" \
  "health server answers on IPv6"
check_log_contains "Starting health server on port 8888" "$LOGS/sidecar-healthcheck-ipv4.log"
check_http_from_pod sidecar-healthcheck-ipv4 "http://0.0.0.0:8888/healthz" \
  "HEALTH_HOST=0.0.0.0 answers on IPv4"
check_http_from_pod_fails sidecar-healthcheck-ipv4 "http://[::1]:8888/healthz" \
  "HEALTH_HOST=0.0.0.0 refuses IPv6"

# ---------------------------------------------------------------------------
section "Verify basic auth"
check_content "allowed" "$OUT/sidecar-basicauth-args/secured.txt"
check_content "allowed" "$OUT/sidecar-basicauth-envfile/secured.txt"

# ---------------------------------------------------------------------------
section "Verify sidecar-5xx files after initial sync"
check_log_matches '/secured.*Not authenticated' "$LOGS/sidecar-5xx.log"
check_content "Hello World!"           "$OUT/sidecar-5xx/hello.world"
check_diff    "$HERE/kubelogo.png"     "$OUT/sidecar-5xx/cm-kubelogo.png"
check_diff    "$HERE/kubelogo.png"     "$OUT/sidecar-5xx/secret-kubelogo.png"
check_diff    "$HERE/kubelogo.png"     "$OUT/sidecar-5xx/url-downloaded-kubelogo.png"
check_content "This absolutely exists" "$OUT/sidecar-5xx/absolute.txt"
check_content "This relatively exists" "$OUT/sidecar-5xx/relative.txt"
check_content "This change-dir exists" "$OUT/sidecar-5xx/change-dir.txt"
check_content "I'm very similar"       "$OUT/sidecar-5xx/similar-configmap.txt"
check_content "I'm very similar"       "$OUT/sidecar-5xx/similar-secret.txt"
check_empty_or_missing                 "$OUT/sidecar-5xx/500.txt"
check_exists  sidecar-5xx /tmp/script_result

# ---------------------------------------------------------------------------
section "Verify script execution counts (initial sync)"
check_log_count "$LOGS/sidecar-pythonscript.log"         "Hello from python script!" 10
check_log_count "$LOGS/sidecar-pythonscript-logfile.log" "Hello from python script!" 10
check_pod_file_exists sidecar-logtofile-pythonscript /opt/logs/sidecar.log

# ---------------------------------------------------------------------------
section "Update ConfigMaps and Secrets"
UPDATE_START=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
sleep 5
kubectl apply -f "$HERE/resources/change_resources.yaml" || exit 1
for p in sidecar sidecar-5xx; do
  for r in "${RESOURCES[@]}"; do wait_for_pod_log "$p" "$r" "$UPDATE_START" || true; done
done
echo "settling for 20s after last log line..."
sleep 20

# ---------------------------------------------------------------------------
section "Verify sidecar files after update"
for f in /tmp/hello.world /tmp/cm-kubelogo.png /tmp/secret-kubelogo.png \
         /tmp/absolute/absolute.txt /tmp/relative/relative.txt \
         /tmp/orig-dir/change-dir.txt /tmp/similar-configmap.txt \
         /tmp/similar-secret.txt; do
  check_not_exists sidecar "$f"
done
for f in /tmp/change-hello.world /tmp/change-cm-kubelogo.png \
         /tmp/change-secret-kubelogo.png /tmp/absolute/change-absolute.txt \
         /tmp/relative/change-relative.txt /tmp/new-dir/change-dir.txt \
         /tmp/change-similar-configmap.txt /tmp/change-similar-secret.txt; do
  check_exists sidecar "$f"
done

# ---------------------------------------------------------------------------
section "Verify script execution counts (cumulative)"
# 10 from the initial sync + 7 from the update.
check_pod_log_count sidecar-logtofile-pythonscript /opt/logs/sidecar.log \
                    "Hello from python script!" 17

# ---------------------------------------------------------------------------
section "Verify 5xx error handling"
check_empty_or_missing "$OUT/sidecar/500.txt"
check_empty_or_missing "$OUT/sidecar-5xx/500.txt"
check_log_contains "Max retries exceeded for URL" "$LOGS/sidecar.log"

summary
