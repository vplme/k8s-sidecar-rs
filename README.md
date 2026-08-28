# k8s-sidecar-rs

A memory-efficient Rust reimplementation of
[kiwigrid/k8s-sidecar](https://github.com/kiwigrid/k8s-sidecar), targeting
**drop-in compatibility**: same environment variables, same CLI flags, same log
messages, so it swaps into an existing pod spec with only an image change.

## Why

The Python implementation costs ~90 MB RSS per container. Measured breakdown on
the reference image (`aarch64`, upstream commit `a61c7ac`):

Measured in-cluster (`make measure-reference`, upstream commit `a61c7ac`,
`aarch64`), reading `VmRSS` of the container's PID 1:

| | reference (Python) | **k8s-sidecar-rs** | improvement |
|---|---|---|---|
| image size | 139 MB | **14.2 MB** | 9.8x |
| RSS idle | 91.0 MB | **4.3 MB** | 21.1x |
| RSS with 50 ConfigMaps x 8 kB | 91.7 MB | **5.5 MB** | 16.7x |

**The cost is fixed, not per-workload.** Syncing 50 ConfigMaps instead of none
adds 0.7 MB. Import-time attribution explains the rest:

| stage | max RSS |
|---|---|
| bare CPython interpreter | 11 MB |
| `import kubernetes` | 107 MB |
| `+ requests`, `+ CoreV1Api`, `+ watch` | 107 MB |

The generated OpenAPI model classes in the `kubernetes` PyPI package account for
**96 of the 107 MB**. Everything else — the sidecar's own logic, HTTP client,
file handling — costs about 11 MB. So the memory is paid at import, before the
sidecar does any work at all, which is why a rewrite wins so decisively and why
most of the win would also be available in Python by dropping the fat client.

## Approach

Behaviour is pinned by a conformance suite *before* any Rust is written. The
suite is a port of upstream's GitHub Actions integration tests
(`.github/workflows/build_and_test.yaml`) into a runnable harness parameterised
by image, so the Python reference and the Rust rewrite are held to exactly the
same contract:

```
make cluster-up           # kind cluster (3 nodes, dual-stack)
make images               # reference, dummy-server and inspector images
make test-all             # everything vs. upstream Python -> baseline
make test-rust            # ported upstream suite vs. this project
make test-ext-rust        # extended suite vs. this project
make measure-rust         # record its image size and RSS
make measure-compare      # reference vs. candidate table
```

There are two suites. `test/run.sh` is the ported upstream suite (55
assertions) and stays faithful to what upstream itself checks. `test/run-ext.sh`
is ours (33 assertions), covering behaviour upstream's CI does not touch:
stale-key and folder-annotation diffing, sha256 write suppression gating
`SCRIPT`, `METHOD=LIST`/`SLEEP`, `NAMESPACE=ALL` and namespace lists,
`RESOURCE_NAME` parsing, `IGNORE_ALREADY_PROCESSED`, `UNIQUE_FILENAMES`,
`DEFAULT_FILE_MODE`, and list pagination.

`make selftest` feeds every extended assertion a deliberately false claim and
requires all of them to fail. An assertion that cannot go red would pass against
a broken implementation, so the oracle itself is tested.

Where upstream behaviour is arguably a bug, **we copy it exactly** and record
the deviation rather than silently improving it — otherwise the suite stops
being a usable differential oracle. See `NOTES.md`.

### Status

- [x] Phase 0 — toolchain and cluster
- [x] Phase 1 — port upstream suite, green against Python (**55/55**)
- [x] Phase 2 — extend suite over the untested surface (**33/33**, self-tested)
- [x] Phase 3 — memory/image-size measurement in-harness (baseline recorded)
- [x] Phase 4 — Rust implementation (**55/55 + 33/33 + selftest green**)
- [x] Phase 5 — lint clean, CI workflow, usage docs

## Using it

Drop-in: replace the image in an existing kiwigrid/k8s-sidecar pod spec.
Every environment variable, CLI flag, and log line documented by
[upstream](https://github.com/kiwigrid/k8s-sidecar#configuration-environment-variables)
behaves identically (deviations: `NOTES.md`, none observable by upstream's
own test suite).

```yaml
containers:
  - name: sidecar
    image: k8s-sidecar-rs:latest   # was: kiwigrid/k8s-sidecar:<version>
    env:
      - {name: LABEL, value: "findme"}
      - {name: FOLDER, value: /data}
      - {name: RESOURCE, value: both}
```

Build it locally with `make rust-image` (host toolchain: rustup with the
`<arch>-unknown-linux-musl` target, musl-tools, cmake). Cross-building is not
supported — aws-lc-sys needs a true musl cross toolchain — so multi-arch
images are produced by CI building natively per architecture
(`.github/workflows/build_and_test.yaml`).

## Implementation

A single ~5 MB static musl binary (`src/`, ~1900 lines) on a busybox base:
one `current_thread` tokio runtime, a task per (resource x namespace) like
upstream's threads, kube-rs for the API (in-cluster auth, watch), reqwest for
`*.url`/`REQ_URL`, and hand-rolled logging that matches upstream's JSON/LOGFMT
output byte-for-byte where the tests grep it. Known deviations are listed in
`NOTES.md`; everything else — including upstream's quirks — is replicated.

## Layout

```
test/
  cluster.sh          create/delete the kind cluster
  build-reference.sh  build upstream Python image from a pinned commit
  run.sh              ported upstream suite   (SIDECAR_IMAGE=... )
  run-ext.sh          extended suite          (SIDECAR_IMAGE=... )
  selftest.sh         negative controls for the extended assertions
  measure.sh          record image size and RSS for one implementation
  measure-compare.sh  print the recorded reference-vs-candidate table
  lib.sh              assertion + wait helpers, shared by both suites
  resources/          upstream manifests, verbatim (image tag substituted at apply time)
  server/             upstream dummy HTTP server
  node-image/         kind node image patched for this kernel
  ext/                extended-suite namespaces, RBAC, pods and inspector image
```

`test/resources/` and `test/server/` are kept **byte-identical to upstream** so
they can be diffed when upstream moves; the sidecar image tag is substituted
with `sed` at apply time rather than edited in place.
