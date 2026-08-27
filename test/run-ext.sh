#!/usr/bin/env bash
# Extended conformance suite -- covers behaviour the upstream CI does not.
#
#   SIDECAR_IMAGE=k8s-sidecar-reference:testing ./test/run-ext.sh
#
# Every case is proven against the Python reference before the Rust build is
# held to it. Assertions run inside a busybox "inspector" container sharing the
# sidecar's target folder, never inside the sidecar itself.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/lib.sh"

SIDECAR_IMAGE="${SIDECAR_IMAGE:-k8s-sidecar-reference:testing}"
INSPECTOR_IMAGE="${INSPECTOR_IMAGE:-ext-inspector:1.0}"
CLUSTER="${CLUSTER:-sidecar-testing}"
MANIFEST_IMAGE="kiwigrid/k8s-sidecar:testing"
export EXT_NS=ext-a
export KUBE_NAMESPACE=ext-a

echo "Image under test : $SIDECAR_IMAGE"

apply() { kubectl apply -f - >/dev/null; }

# ---------------------------------------------------------------------------
section "Set up extended-suite namespaces, RBAC and pods"
kind load docker-image "$SIDECAR_IMAGE"   --name "$CLUSTER" >/dev/null 2>&1
kind load docker-image "$INSPECTOR_IMAGE" --name "$CLUSTER" >/dev/null 2>&1
kubectl apply -f "$HERE/ext/infra.yaml" >/dev/null

# Pods and watched resources must not survive between runs: a resource that
# already exists when the sidecar starts is absorbed by the initial LIST sync
# instead of arriving as a watch event, which changes SCRIPT invocation counts.
kubectl delete -f "$HERE/ext/pods.yaml"      --ignore-not-found --wait --timeout=180s >/dev/null 2>&1
kubectl delete -f "$HERE/ext/pods-list.yaml" --ignore-not-found --wait --timeout=120s >/dev/null 2>&1
for l in extdiff extgate extuniq extmode extsleep extall extmulti extrn extign extpage extlist; do
  kubectl delete configmap,secret -n ext-a -l "$l" --ignore-not-found >/dev/null 2>&1
  kubectl delete configmap,secret -n ext-b -l "$l" --ignore-not-found >/dev/null 2>&1
done
# T9's resources are selected by name, not label, so they need explicit removal.
kubectl delete configmap -n ext-a rn-one rn-two rn-three --ignore-not-found >/dev/null 2>&1

sed "s|${MANIFEST_IMAGE}|${SIDECAR_IMAGE}|g" "$HERE/ext/pods.yaml" | apply
for p in ext-diff ext-gate ext-unique ext-mode ext-sleep ext-allns \
         ext-multins ext-resname ext-ignore; do
  wait_for_pod_ready "$p" 180 || exit 1
done
KUBE_NAMESPACE=ext-b wait_for_pod_ready ext-page 180 || exit 1
# Pods report ready before the watch stream is actually established.
echo "settling for 15s..."; sleep 15

# ---------------------------------------------------------------------------
section "T1 -- stale-key removal and folder-annotation changes"

echo "-- create d-cm with two keys"
apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: d-cm, namespace: ext-a, labels: {extdiff: "yes"}}
data: {a.txt: "A", b.txt: "B"}
EOF
ext_content ext-diff /data/a.txt A "T1.1 a.txt written"
ext_content ext-diff /data/b.txt B "T1.1 b.txt written"

echo "-- drop key b.txt from the ConfigMap"
apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: d-cm, namespace: ext-a, labels: {extdiff: "yes"}}
data: {a.txt: "A"}
EOF
ext_exists ext-diff /data/a.txt "T1.2 a.txt kept"
ext_absent ext-diff /data/b.txt "T1.2 b.txt removed when key dropped"

echo "-- add a folder-override annotation"
apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata:
  name: d-cm
  namespace: ext-a
  labels: {extdiff: "yes"}
  annotations: {k8s-sidecar-target-directory: "/data/sub"}
data: {a.txt: "A"}
EOF
ext_content ext-diff /data/sub/a.txt A "T1.3 a.txt written to annotated folder"
ext_absent  ext-diff /data/a.txt        "T1.3 a.txt removed from old folder"

echo "-- delete the ConfigMap"
kubectl delete configmap d-cm -n ext-a >/dev/null
ext_absent ext-diff /data/sub/a.txt "T1.4 a.txt removed on ConfigMap delete"

# ---------------------------------------------------------------------------
section "T2 -- sha256 suppression gates SCRIPT execution"

echo "-- create g-cm"
apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: g-cm, namespace: ext-a, labels: {extgate: "yes"}}
data: {x.txt: "1"}
EOF
ext_content ext-gate /data/x.txt 1 "T2.1 x.txt written"
ext_count   ext-gate /out/script.log SCRIPT_RAN 1 "T2.1 script ran once"

echo "-- bump an annotation, leave data identical (MODIFIED event, no content change)"
apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata:
  name: g-cm
  namespace: ext-a
  labels: {extgate: "yes"}
  annotations: {touch: "1"}
data: {x.txt: "1"}
EOF
ext_count_stable ext-gate /out/script.log SCRIPT_RAN 1 \
  "T2.2 unchanged content does not re-run script"

echo "-- change the data"
apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata:
  name: g-cm
  namespace: ext-a
  labels: {extgate: "yes"}
  annotations: {touch: "1"}
data: {x.txt: "2"}
EOF
ext_content ext-gate /data/x.txt 2 "T2.3 x.txt updated"
ext_count   ext-gate /out/script.log SCRIPT_RAN 2 "T2.3 changed content re-runs script"

# ---------------------------------------------------------------------------
section "T3 -- UNIQUE_FILENAMES naming"

apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: u-cm, namespace: ext-a, labels: {extuniq: "yes"}}
data: {u.txt: "from-cm"}
---
apiVersion: v1
kind: Secret
metadata: {name: u-sec, namespace: ext-a, labels: {extuniq: "yes"}}
stringData: {u.txt: "from-secret"}
EOF
ext_content ext-unique "/data/namespace_ext-a.configmap_u-cm.u.txt" from-cm \
  "T3.1 configmap key gets unique name"
ext_content ext-unique "/data/namespace_ext-a.secret_u-sec.u.txt" from-secret \
  "T3.2 secret key with same name does not collide"
ext_absent ext-unique /data/u.txt "T3.3 plain filename not used"

# ---------------------------------------------------------------------------
section "T4 -- DEFAULT_FILE_MODE"

apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: m-cm, namespace: ext-a, labels: {extmode: "yes"}}
data: {m.txt: "M"}
EOF
ext_exists ext-mode /data/m.txt "T4.1 m.txt written"
ext_mode   ext-mode /data/m.txt 400 "T4.2 DEFAULT_FILE_MODE=400 applied"

# ---------------------------------------------------------------------------
section "T5 -- METHOD=LIST syncs once and exits"

# LIST has no watch, so the ConfigMap must exist before the pod starts.
apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: l-cm, namespace: ext-a, labels: {extlist: "yes"}}
data: {l.txt: "L"}
EOF
sed "s|${MANIFEST_IMAGE}|${SIDECAR_IMAGE}|g" "$HERE/ext/pods-list.yaml" | apply
until [ "$(kubectl get pod ext-list -n ext-a -o jsonpath='{.status.phase}' 2>/dev/null)" = "Running" ]; do
  sleep 2
done
ext_content ext-list /data/l.txt L "T5.1 LIST wrote the file"
# The sidecar process must terminate; only the inspector keeps running.
if _spin ext-list "true" && \
   [ "$(kubectl get pod ext-list -n ext-a \
        -o jsonpath='{.status.containerStatuses[?(@.name=="sidecar")].state.terminated.exitCode}')" = "0" ]; then
  _pass "T5.2 LIST exits cleanly after syncing"
else
  _fail "T5.2 LIST exits cleanly after syncing" \
        "state: $(kubectl get pod ext-list -n ext-a -o jsonpath='{.status.containerStatuses[?(@.name=="sidecar")].state}')"
fi

# ---------------------------------------------------------------------------
section "T6 -- METHOD=SLEEP polls for changes"

apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: s-cm, namespace: ext-a, labels: {extsleep: "yes"}}
data: {s.txt: "S"}
EOF
ext_content ext-sleep /data/s.txt S "T6.1 SLEEP picks up a new ConfigMap"
kubectl delete configmap s-cm -n ext-a >/dev/null
ext_absent ext-sleep /data/s.txt "T6.2 SLEEP picks up a deletion on the next poll"

# ---------------------------------------------------------------------------
section "T7/T8 -- namespace scoping"

apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: na-cm, namespace: ext-a, labels: {extall: "yes", extmulti: "yes"}}
data: {ns-a.txt: "in-a"}
---
apiVersion: v1
kind: ConfigMap
metadata: {name: nb-cm, namespace: ext-b, labels: {extall: "yes", extmulti: "yes"}}
data: {ns-b.txt: "in-b"}
EOF
ext_content ext-allns   /data/ns-a.txt in-a "T7.1 NAMESPACE=ALL collects from ext-a"
ext_content ext-allns   /data/ns-b.txt in-b "T7.2 NAMESPACE=ALL collects from ext-b"
ext_content ext-multins /data/ns-a.txt in-a "T8.1 NAMESPACE=ext-a,ext-b collects from ext-a"
ext_content ext-multins /data/ns-b.txt in-b "T8.2 NAMESPACE=ext-a,ext-b collects from ext-b"

# ---------------------------------------------------------------------------
section "T9 -- RESOURCE_NAME selects by name and ignores the label"

# rn-one/two/three are deliberately UNLABELLED: with RESOURCE_NAME set the
# sidecar reads by name and never applies the label selector. rn-other carries
# the label but is not named, so it must be ignored.
apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: rn-one, namespace: ext-a}
data: {rn1.txt: "one"}
---
apiVersion: v1
kind: ConfigMap
metadata: {name: rn-two, namespace: ext-a}
data: {rn2.txt: "two"}
---
apiVersion: v1
kind: ConfigMap
metadata: {name: rn-three, namespace: ext-a}
data: {rn3.txt: "three"}
---
apiVersion: v1
kind: ConfigMap
metadata: {name: rn-other, namespace: ext-a, labels: {extrn: "yes"}}
data: {rnx.txt: "other"}
EOF
ext_content ext-resname /data/rn1.txt one   "T9.1 bare name form"
ext_content ext-resname /data/rn2.txt two   "T9.2 configmap/name form"
ext_content ext-resname /data/rn3.txt three "T9.3 namespace/configmap/name form"
ext_absent  ext-resname /data/rnx.txt       "T9.4 labelled but unnamed resource ignored"

# ---------------------------------------------------------------------------
section "T10 -- IGNORE_ALREADY_PROCESSED skips unchanged resourceVersions"

apply <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata: {name: i-cm, namespace: ext-a, labels: {extign: "yes"}}
data: {i.txt: "I"}
EOF
ext_content     ext-ignore /data/i.txt I "T10.1 file written on first pass"
ext_log_contains ext-ignore "Ignoring configmap ext-a/i-cm" \
  "T10.2 subsequent passes skip the unchanged resource"

# ---------------------------------------------------------------------------
section "T11 -- list pagination (limit=5, 12 resources)"

EXT_NS=ext-b
for i in $(seq -w 1 12); do
  cat <<EOF | apply
apiVersion: v1
kind: ConfigMap
metadata: {name: page-cm-$i, namespace: ext-b, labels: {extpage: "yes"}}
data: {page-$i.txt: "p$i"}
EOF
done
missing=0
for i in $(seq -w 1 12); do
  _spin ext-page "test -e /data/page-$i.txt" || missing=$((missing + 1))
done
if [ "$missing" -eq 0 ]; then _pass "T11.1 all 12 paginated resources synced"
else _fail "T11.1 all 12 paginated resources synced" "$missing of 12 missing"; fi
ext_content ext-page /data/page-12.txt p12 "T11.2 last page's content correct"
EXT_NS=ext-a

summary
