# Inspector container for the extended suite: a shell, coreutils and tar so the
# harness can exec/cp into the pod regardless of what the sidecar image holds.
#
# Built locally rather than pulled at pod-schedule time: the kind nodes have no
# registry access, and `kind load docker-image busybox:1.37` fails on that
# image's attestation manifests ("content digest ... not found"). A local build
# with provenance/sbom disabled produces a plain single-platform image that
# loads cleanly.
FROM busybox:1.37
CMD ["sh", "-c", "sleep infinity"]
