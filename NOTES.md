# Engineering notes

## Environment deviations

These are workarounds for *this* machine, not upstream bugs.

1. **`/dev/kmsg` missing.** This kernel does not expose `/dev/kmsg`, so kubelet
   inside a kind node dies at startup with
   `failed to create kubelet: open /dev/kmsg: no such file or directory`.
   `test/node-image/Dockerfile` derives from `kindest/node` and adds a kubelet
   systemd drop-in that recreates the symlink to `/dev/console` before every
   start.

2. **Multi-arch resolution.** `docker pull kiwigrid/k8s-sidecar` resolves to
   `linux/arm/v7` on this `aarch64` host even with `--platform linux/arm64`
   (`alpine` resolves correctly, so it is that manifest list). We build the
   reference image from source instead, which is what upstream CI does anyway.

3. **Upstream Dockerfile bit-rot.** It pins `libcrypto3=3.5.7-r0` and
   `libssl3=3.5.7-r0`; those revisions have been superseded and purged from
   `dl-cdn.alpinelinux.org`, so the build no longer reproduces. `build-reference.sh`
   strips the `=<version>` pins. Affects only the reference image's OpenSSL
   patch level, not sidecar behaviour.

4. **Case-insensitive project filesystem.** `/Users/vpl/Repos` is a
   case-preserving but case-insensitive bind mount. One file can appear twice
   in a directory listing under different casings (`Dockerfile` and
   `dockerfile`, same inode). Do not "clean up" the apparent duplicate: `rm`
   deletes the single real file, and the mount is then left incoherent — `ls`
   and `test -f` still report the file from cached dentries while `open()`
   returns ENOENT, so the deletion looks like it did nothing. Recovering means
   removing and recreating the parent directory. Avoid paths differing only by
   case anywhere in this repo, and verify deletions with `cat`, not `ls`.

## Harness bugs found and fixed

- **`pipefail` + `grep -q` SIGPIPE.** `kubectl logs "$pod" | grep -q "$pattern"`
  cannot be used under `set -o pipefail`: `grep -q` exits on first match, sends
  SIGPIPE to `kubectl logs`, and the pipeline reports failure even though the
  pattern *was* found. Only bites on pods whose logs are large enough that grep
  exits before kubectl finishes writing — i.e. exactly the pods you care about.
  `wait_for_pod_log` captures into a variable instead.

- **Watched resources must not survive between runs.** If the ConfigMaps and
  Secrets from `resources.yaml` already exist when the sidecar pods start, the
  initial LIST sync absorbs them in one batch — a *single* `SCRIPT` invocation
  — instead of arriving as individual watch events. Script-execution counts
  then come out at 2 instead of 10. `run.sh` deletes them up front.

## Upstream behaviours to replicate exactly

Decision: copy bug-for-bug, document here, revisit deliberately later.

- On a failed `*.url` fetch the sidecar writes an **empty file**, it does not
  skip the write. (`_get_file_data_and_name` returns `b""`/`""`, and `request()`
  returns a dummy object with empty `.text`/`.content` on exhausted retries.)
- `remove_file` logs at **error** level when the file is already gone.
- Basic-auth credentials are encoded **`latin1`** by default, not UTF-8
  (`REQ_BASIC_AUTH_ENCODING` overrides).
- A file whose content is unchanged is **not rewritten**, and the resulting
  `files_changed == false` suppresses both `SCRIPT` and the `REQ_URL` call.
  Getting this wrong causes webhook storms.
- `_get_destination_folder` joins a *relative* folder annotation onto `FOLDER`;
  an absolute one replaces it.
- Deleting a data key from a ConfigMap deletes its file; changing the folder
  annotation deletes the file from the **old** folder and writes to the new one.
- `RESOURCE_NAME` entries are split on `/` and **reversed**, accepting
  `name`, `type/name`, `namespace/type/name`. Setting it downgrades
  `METHOD=WATCH` to `SLEEP` polling.
- `LIST` mode paginates with `limit=5` and `_continue`.

### Known irreducible mismatch

`DISABLE_X509_STRICT_VERIFICATION` is a workaround for Python 3.13+ OpenSSL
`VERIFY_X509_STRICT`. It has no rustls equivalent. Plan: accept the variable and
log a warning that it is a no-op.

## Coverage beyond the ported upstream suite

`test/run-ext.sh` closes the gaps upstream CI leaves. All 33 assertions are
green against the Python reference; the mapping from gap to case is:

| case | covers |
|---|---|
| T1 | stale-key removal, folder-annotation change moves files and deletes from the old folder, delete-on-ConfigMap-delete |
| T2 | sha256 write suppression gating `SCRIPT` (unchanged content must not re-fire) |
| T3 | `UNIQUE_FILENAMES` naming, ConfigMap vs. Secret disambiguation |
| T4 | `DEFAULT_FILE_MODE` |
| T5 | `METHOD=LIST` syncs once and exits 0 |
| T6 | `METHOD=SLEEP` polls for both additions and deletions |
| T7 | `NAMESPACE=ALL` |
| T8 | comma-separated namespace list |
| T9 | `RESOURCE_NAME` in all three forms; label selector ignored when set |
| T10 | `IGNORE_ALREADY_PROCESSED` skips unchanged resourceVersions |
| T11 | list pagination (`limit=5` + `_continue`) across 12 resources |

`REQ_URL` firing is not separately tested: it is gated by the same
`files_changed` flag as `SCRIPT`, which T2 covers, and the ported upstream suite
already exercises the request path itself.

### The oracle is itself tested

`test/selftest.sh` gives each assertion type a false claim and requires all six
to fail. This exists because the failure mode of a black-box suite is silent:
an assertion pointed at the wrong container or a helper that swallows its own
error passes against every implementation, including a broken one. Run it
whenever an assertion helper changes.

### Notes discovered while building the extended suite

- Assertions must run in the **inspector** container, never the sidecar — see
  the Phase 4 constraints below.
- With `RESOURCE_NAME` set, the label selector is not applied at all; resources
  are read by name. T9 proves this by leaving the named ConfigMaps unlabelled
  and labelling a ConfigMap that is *not* named.
- `METHOD=LIST` leaves the pod at `1/2 Running` once the sidecar exits, so it
  must never be waited on with `--for=condition=ready`.
- `kind load docker-image busybox:1.37` fails with `content digest ... not
  found` (attestation manifests). The inspector image is therefore built
  locally with `--provenance=false --sbom=false`.

## Phase 4 constraints discovered early

**The Rust image cannot be `scratch`.** `SCRIPT` is part of the drop-in
contract, and `helpers.execute()` runs a non-executable script as `sh <path>`.
Supporting it requires a shell in the sidecar image, so the base is busybox
(~4 MB), not `scratch`. Target image is therefore ~10-14 MB rather than ~8 MB —
still an order of magnitude below the reference's 139 MB. A side benefit is that
busybox supplies `sh`/`test`/`grep`/`tar`, so `kubectl exec` and `kubectl cp`
keep working against the Rust image.

**The extended suite still uses an inspector container.** Each `test/ext` pod
runs a busybox `inspector` alongside the sidecar, sharing the target folder via
an `emptyDir`, and all assertions exec into the inspector rather than the
sidecar. Two reasons: it keeps the suite a genuine black-box test that does not
depend on what the sidecar image happens to contain, and it is the only way to
inspect `METHOD=LIST`, where the sidecar container exits as soon as it finishes.

**The ported upstream suite will need one substitution for the Rust image.**
`test/resources/sidecar.yaml` drives four pods with a `#!/usr/bin/env python`
script, which exercises upstream's image contents rather than sidecar
behaviour. Those pods need an `sh` equivalent when the suite runs against a
non-Python image. To be handled in Phase 4, not by editing the vendored
manifests.
