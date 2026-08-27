# k8s-sidecar-rs -- a memory-efficient Rust reimplementation of kiwigrid/k8s-sidecar.
#
# Phase 1 targets: stand up a kind cluster and prove the conformance suite green
# against the upstream Python implementation before any Rust is written.

REFERENCE_IMAGE ?= k8s-sidecar-reference:testing
RUST_IMAGE      ?= k8s-sidecar-rs:testing
DUMMY_IMAGE     ?= dummy-server:1.0.0
CLUSTER         ?= sidecar-testing
K8S_VERSION     ?= v1.34.3

.PHONY: help cluster-up cluster-down images reference-image dummy-image \
        test-reference test-rust clean

help:
	@echo "cluster-up       create the kind test cluster ($(K8S_VERSION))"
	@echo "cluster-down     delete it"
	@echo "images           build the reference and dummy-server images"
	@echo "test-reference   run the conformance suite against upstream Python"
	@echo "test-rust        run the conformance suite against the Rust build"

cluster-up:
	CLUSTER=$(CLUSTER) ./test/cluster.sh up $(K8S_VERSION)

cluster-down:
	CLUSTER=$(CLUSTER) ./test/cluster.sh down

images: reference-image dummy-image

reference-image:
	REFERENCE_IMAGE=$(REFERENCE_IMAGE) ./test/build-reference.sh

dummy-image:
	cp -f test/kubelogo.png test/server/static/
	docker build -t $(DUMMY_IMAGE) test/server

test-reference:
	SIDECAR_IMAGE=$(REFERENCE_IMAGE) DUMMY_IMAGE=$(DUMMY_IMAGE) \
	  CLUSTER=$(CLUSTER) ./test/run.sh

test-rust:
	SIDECAR_IMAGE=$(RUST_IMAGE) DUMMY_IMAGE=$(DUMMY_IMAGE) \
	  CLUSTER=$(CLUSTER) ./test/run.sh

clean:
	rm -rf test/.out
