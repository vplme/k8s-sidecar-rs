# k8s-sidecar-rs -- a memory-efficient Rust reimplementation of kiwigrid/k8s-sidecar.
#
# Phase 1 targets: stand up a kind cluster and prove the conformance suite green
# against the upstream Python implementation before any Rust is written.

REFERENCE_IMAGE ?= k8s-sidecar-reference:testing
RUST_IMAGE      ?= k8s-sidecar-rs:testing
DUMMY_IMAGE     ?= dummy-server:1.0.0
INSPECTOR_IMAGE ?= ext-inspector:1.0
CLUSTER         ?= sidecar-testing
K8S_VERSION     ?= v1.34.3

.PHONY: help cluster-up cluster-down images reference-image dummy-image \
        inspector-image rust-image test-reference test-rust test-ext-reference \
        test-ext-rust selftest test-all measure-reference measure-rust \
        measure-compare clean

help:
	@echo "cluster-up          create the kind test cluster ($(K8S_VERSION))"
	@echo "cluster-down        delete it"
	@echo "images              build reference, dummy-server and inspector images"
	@echo ""
	@echo "test-reference      ported upstream suite  vs. upstream Python"
	@echo "test-ext-reference  extended suite         vs. upstream Python"
	@echo "test-rust           ported upstream suite  vs. the Rust build"
	@echo "test-ext-rust       extended suite         vs. the Rust build"
	@echo "selftest            prove the extended assertions can fail"
	@echo "test-all            reference + extended + selftest against Python"
	@echo ""
	@echo "measure-reference   record image size and RSS for upstream Python"
	@echo "measure-rust        record image size and RSS for the Rust build"
	@echo "measure-compare     print the recorded comparison"

cluster-up:
	CLUSTER=$(CLUSTER) ./test/cluster.sh up $(K8S_VERSION)

cluster-down:
	CLUSTER=$(CLUSTER) ./test/cluster.sh down

images: reference-image dummy-image inspector-image

reference-image:
	REFERENCE_IMAGE=$(REFERENCE_IMAGE) ./test/build-reference.sh

dummy-image:
	cp -f test/kubelogo.png test/server/static/
	docker build -t $(DUMMY_IMAGE) test/server

test-reference:
	SIDECAR_IMAGE=$(REFERENCE_IMAGE) DUMMY_IMAGE=$(DUMMY_IMAGE) \
	  CLUSTER=$(CLUSTER) ./test/run.sh

# Build the static musl binary on the host (incremental compilation keeps the
# loop fast), then wrap it in the busybox image. `--build-context build=` makes
# `COPY --from=build` read the host binary instead of running the Dockerfile's
# in-image build stage. The context is staged outside the repo mount (its
# dentry-cache quirks corrupt docker build contexts and cargo build dirs alike).
rust-image:
	cargo build --release --target aarch64-unknown-linux-musl
	rm -rf /tmp/k8s-sidecar-rs-ctx && mkdir -p /tmp/k8s-sidecar-rs-ctx
	cp Dockerfile "$$HOME/.cache/k8s-sidecar-rs-target/aarch64-unknown-linux-musl/release/k8s-sidecar-rs" /tmp/k8s-sidecar-rs-ctx/
	docker build --provenance=false --sbom=false \
	  --build-context build=/tmp/k8s-sidecar-rs-ctx \
	  -t $(RUST_IMAGE) /tmp/k8s-sidecar-rs-ctx

test-rust: rust-image
	SIDECAR_IMAGE=$(RUST_IMAGE) DUMMY_IMAGE=$(DUMMY_IMAGE) \
	  CLUSTER=$(CLUSTER) SCRIPT_FLAVOR=sh ./test/run.sh

inspector-image:
	docker build -q --provenance=false --sbom=false \
	  -t $(INSPECTOR_IMAGE) test/ext/inspector

test-ext-reference:
	SIDECAR_IMAGE=$(REFERENCE_IMAGE) INSPECTOR_IMAGE=$(INSPECTOR_IMAGE) \
	  CLUSTER=$(CLUSTER) ./test/run-ext.sh

test-ext-rust: rust-image
	SIDECAR_IMAGE=$(RUST_IMAGE) INSPECTOR_IMAGE=$(INSPECTOR_IMAGE) \
	  CLUSTER=$(CLUSTER) ./test/run-ext.sh

selftest:
	./test/selftest.sh

# Extended suite before selftest: selftest inspects the pods it leaves behind.
test-all: test-reference test-ext-reference selftest

measure-reference:
	SIDECAR_IMAGE=$(REFERENCE_IMAGE) CLUSTER=$(CLUSTER) ./test/measure.sh

measure-rust:
	SIDECAR_IMAGE=$(RUST_IMAGE) CLUSTER=$(CLUSTER) ./test/measure.sh

measure-compare:
	./test/measure-compare.sh

clean:
	rm -rf test/.out
