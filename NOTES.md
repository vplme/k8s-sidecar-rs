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

## Phase 2 — gaps in the ported suite

Upstream CI does not cover these, and each is easy to get wrong in a rewrite:

| area | why it matters |
|---|---|
| stale-key / folder-change diffing | the gnarliest logic upstream (`resources.py`); only partially exercised |
| sha256 write suppression | gates `SCRIPT` and `REQ_URL`; wrong => webhook storm |
| `METHOD=LIST`, `METHOD=SLEEP` | only `WATCH` is meaningfully tested |
| `NAMESPACE=ALL`, comma-separated namespaces | cross-namespace cache scoping |
| `RESOURCE_NAME` parsing | reversed-split rules, silent WATCH->SLEEP downgrade |
| `IGNORE_ALREADY_PROCESSED` | resourceVersion caching |
| `UNIQUE_FILENAMES` | `namespace_X.configmap_Y.file` naming |
| `DEFAULT_FILE_MODE` | chmod after write |
| list pagination | `limit=5` + `_continue` |

## Phase 4 constraint discovered early

The suite reaches into the sidecar container with `kubectl exec` (`sh`, `test`,
`ls`, `grep`) and `kubectl cp` (needs `tar`). A `scratch`/distroless-static Rust
image has none of these, so the suite could not inspect it.

Preferred fix: add a small busybox **inspector container** to each test pod,
sharing the sidecar's target folder via an `emptyDir`, and exec into *that*.
This keeps the shipped Rust image minimal, exercises the real deployment pattern
(shared volume with an app container), and works identically for both
implementations. To be done as part of the Phase 2 manifest changes.
