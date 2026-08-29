# Multi-stage build for k8s-sidecar-rs: a hermetic `docker build .` produces
# the release image from source.
#
# The build stage compiles natively per arch (CI and the release workflow run
# it on a native runner per platform; no cross-compiling, as aws-lc-sys needs
# a true musl cross toolchain). The local dev loop skips the stage entirely:
# `make rust-image` builds the binary on the host for incremental compilation
# and substitutes it with `--build-context build=<dir>`.
FROM rust:1-alpine AS build
# gcc/musl-dev for the linker and libc headers; cmake/make for aws-lc-sys.
RUN apk add --no-cache musl-dev gcc make cmake
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# Cache mounts keep the cargo registry and incremental build state out of the
# image; the binary is copied out because the target dir vanishes with the
# mount. rust:alpine targets musl with crt-static by default, so this is the
# same static binary the Makefile builds on the host.
RUN --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked \
    && cp target/release/k8s-sidecar-rs /k8s-sidecar-rs

# Runtime image.
#
# busybox rather than scratch: SCRIPT is part of the drop-in contract and
# upstream runs non-executable scripts via `sh <path>`, so a shell must exist.
# busybox also supplies test/grep/tar, keeping `kubectl exec`/`kubectl cp`
# usable against this image (the conformance suite relies on that).
#
# Base alpine ships the ca-certificates bundle; busybox does not, and
# reqwest/rustls refuses to build a client when no system roots exist
# ("No CA certificates were loaded from the system"). Python's image gets its
# roots from certifi; this is the equivalent.
FROM alpine:3.22 AS certs

FROM busybox:1.37
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
LABEL org.opencontainers.image.source=https://github.com/vplme/k8s-sidecar-rs
LABEL org.opencontainers.image.description="Rust reimplementation of kiwigrid/k8s-sidecar"
COPY --from=build /k8s-sidecar-rs /k8s-sidecar
# Match upstream: run as the nobody user to satisfy MustRunAsNonRoot policies.
USER 65534:65534
ENTRYPOINT ["/k8s-sidecar"]
