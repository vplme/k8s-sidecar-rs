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
        inspector-image test-reference test-rust test-ext-reference \
        test-ext-rust selftest test-all clean

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

test-rust:
	SIDECAR_IMAGE=$(RUST_IMAGE) DUMMY_IMAGE=$(DUMMY_IMAGE) \
	  CLUSTER=$(CLUSTER) ./test/run.sh

inspector-image:
	docker build -q --provenance=false --sbom=false \
	  -t $(INSPECTOR_IMAGE) test/ext/inspector

test-ext-reference:
	SIDECAR_IMAGE=$(REFERENCE_IMAGE) INSPECTOR_IMAGE=$(INSPECTOR_IMAGE) \
	  CLUSTER=$(CLUSTER) ./test/run-ext.sh

test-ext-rust:
	SIDECAR_IMAGE=$(RUST_IMAGE) INSPECTOR_IMAGE=$(INSPECTOR_IMAGE) \
	  CLUSTER=$(CLUSTER) ./test/run-ext.sh

selftest:
	./test/selftest.sh

# Extended suite before selftest: selftest inspects the pods it leaves behind.
test-all: test-reference test-ext-reference selftest

clean:
	rm -rf test/.out
