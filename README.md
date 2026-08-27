# k8s-sidecar-rs

A memory-efficient Rust reimplementation of
[kiwigrid/k8s-sidecar](https://github.com/kiwigrid/k8s-sidecar), targeting
**drop-in compatibility**: same environment variables, same CLI flags, same log
messages, so it swaps into an existing pod spec with only an image change.

## Why

The Python implementation costs ~90 MB RSS per container. Measured breakdown on
the reference image (`aarch64`, upstream commit `a61c7ac`):

| stage                        | max RSS |
|------------------------------|---------|
| bare CPython interpreter     | 11 MB   |
| `import kubernetes`          | 107 MB  |
| `import requests`            | 107 MB  |
| `CoreV1Api` + `watch`        | 107 MB  |

The generated OpenAPI model classes in the `kubernetes` PyPI package account for
**96 of the 107 MB**. Everything else — the sidecar's own logic, HTTP client,
file handling — costs about 11 MB.

Image size: 139 MB (reference build) vs. a target of ~10 MB for a static Rust
binary on `scratch`.

## Approach

Behaviour is pinned by a conformance suite *before* any Rust is written. The
suite is a port of upstream's GitHub Actions integration tests
(`.github/workflows/build_and_test.yaml`) into a runnable harness parameterised
by image, so the Python reference and the Rust rewrite are held to exactly the
same contract:

```
make cluster-up        # kind cluster (3 nodes, dual-stack)
make images            # build reference + dummy-server images
make test-reference    # suite vs. upstream Python  -> baseline
make test-rust         # suite vs. this project
```

Where upstream behaviour is arguably a bug, **we copy it exactly** and record
the deviation rather than silently improving it — otherwise the suite stops
being a usable differential oracle. See `NOTES.md`.

### Status

- [x] Phase 0 — toolchain and cluster
- [x] Phase 1 — port upstream suite, green against Python (**55/55**)
- [ ] Phase 2 — extend suite to cover the untested surface
- [ ] Phase 3 — memory/image-size measurement in-harness
- [ ] Phase 4 — Rust implementation
- [ ] Phase 5 — compare, tune, document

## Layout

```
test/
  cluster.sh          create/delete the kind cluster
  build-reference.sh  build upstream Python image from a pinned commit
  run.sh              the conformance suite (SIDECAR_IMAGE=... )
  lib.sh              assertion + wait helpers
  resources/          upstream manifests, verbatim (image tag substituted at apply time)
  server/             upstream dummy HTTP server
  node-image/         kind node image patched for this kernel
```

`test/resources/` and `test/server/` are kept **byte-identical to upstream** so
they can be diffed when upstream moves; the sidecar image tag is substituted
with `sed` at apply time rather than edited in place.
