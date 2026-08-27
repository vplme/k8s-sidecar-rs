#!/usr/bin/env bash
# Create / delete the kind cluster used by the conformance suite.
#
#   ./test/cluster.sh up     [k8s-version]
#   ./test/cluster.sh down
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CLUSTER="${CLUSTER:-sidecar-testing}"
K8S_VERSION="${2:-${K8S_VERSION:-v1.34.3}}"
NODE_IMAGE="kind-node-kmsg:${K8S_VERSION}"

case "${1:-up}" in
  up)
    # kindest/node needs /dev/kmsg, which this kernel does not expose; the
    # derived image adds a kubelet drop-in that recreates it. See
    # test/node-image/Dockerfile.
    docker build -q --build-arg "BASE=kindest/node:${K8S_VERSION}" \
      -t "$NODE_IMAGE" "$HERE/node-image" >/dev/null
    kind create cluster --name "$CLUSTER" --config "$HERE/kind-config.yaml" \
      --image "$NODE_IMAGE" --wait 5m
    kubectl get nodes
    ;;
  down)
    kind delete cluster --name "$CLUSTER"
    ;;
  *)
    echo "usage: $0 {up|down} [k8s-version]" >&2; exit 2 ;;
esac
