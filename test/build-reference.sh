#!/usr/bin/env bash
# Builds the upstream kiwigrid/k8s-sidecar image from source at a pinned commit.
#
# We build rather than pull because the published multi-arch manifest resolves to
# linux/arm/v7 on aarch64 hosts, and because upstream CI tests a source build.
#
# DEVIATION FROM UPSTREAM: the upstream Dockerfile pins exact Alpine package
# revisions (libcrypto3=3.5.7-r0, libssl3=3.5.7-r0). Those revisions have since
# been superseded and purged from dl-cdn.alpinelinux.org, so the build no longer
# reproduces. We strip the "=<version>" pins, keeping the packages themselves.
# This affects only the OpenSSL patch level of the reference image, not sidecar
# behaviour under test.
set -euo pipefail

UPSTREAM_REPO="${UPSTREAM_REPO:-https://github.com/kiwigrid/k8s-sidecar.git}"
UPSTREAM_REF="${UPSTREAM_REF:-a61c7ac31c826c5e35efe53fbbcae93e1791799f}"
REFERENCE_IMAGE="${REFERENCE_IMAGE:-k8s-sidecar-reference:testing}"
WORKDIR="${WORKDIR:-$(cd "$(dirname "$0")" && pwd)/.upstream}"

if [ ! -d "$WORKDIR/.git" ]; then
  rm -rf "$WORKDIR"
  git init -q "$WORKDIR"
  git -C "$WORKDIR" remote add origin "$UPSTREAM_REPO"
fi
git -C "$WORKDIR" fetch -q --depth 1 origin "$UPSTREAM_REF"
git -C "$WORKDIR" checkout -q -f FETCH_HEAD

# Unpin the purged Alpine package revisions (see DEVIATION above).
sed -E 's/(libcrypto3|libssl3)=[0-9][^ \\]*/\1/g' \
    "$WORKDIR/Dockerfile" > "$WORKDIR/Dockerfile.unpinned"

echo "Building $REFERENCE_IMAGE from upstream $UPSTREAM_REF"
docker build -f "$WORKDIR/Dockerfile.unpinned" -t "$REFERENCE_IMAGE" "$WORKDIR"
