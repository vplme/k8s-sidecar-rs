# Runtime image for k8s-sidecar-rs.
#
# busybox rather than scratch: SCRIPT is part of the drop-in contract and
# upstream runs non-executable scripts via `sh <path>`, so a shell must exist.
# busybox also supplies test/grep/tar, keeping `kubectl exec`/`kubectl cp`
# usable against this image (the conformance suite relies on that).
#
# The binary is built on the host by `make rust-image` (static musl, ~5 MB);
# building the Rust toolchain inside Docker would only slow the loop down.
#
# Base alpine ships the ca-certificates bundle; busybox does not, and
# reqwest/rustls refuses to build a client when no system roots exist
# ("No CA certificates were loaded from the system"). Python's image gets its
# roots from certifi; this is the equivalent.
FROM alpine:3.22 AS certs

FROM busybox:1.37
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
LABEL org.opencontainers.image.source=https://github.com/vpl/k8s-sidecar-rs
LABEL org.opencontainers.image.description="Rust reimplementation of kiwigrid/k8s-sidecar"
COPY k8s-sidecar-rs /k8s-sidecar
# Match upstream: run as the nobody user to satisfy MustRunAsNonRoot policies.
USER 65534:65534
ENTRYPOINT ["/k8s-sidecar"]
