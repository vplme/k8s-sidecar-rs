# Derived kind node image for environments whose kernel does not expose /dev/kmsg
# (nested containers / some VM sandboxes). Without it kubelet fails at startup with
#   failed to create kubelet: open /dev/kmsg: no such file or directory
# The drop-in re-creates the symlink before every kubelet start, including restarts.
ARG BASE=kindest/node:v1.34.3
FROM ${BASE}
RUN mkdir -p /etc/systemd/system/kubelet.service.d && \
    printf '%s\n' \
      '[Service]' \
      "ExecStartPre=/bin/sh -c '[ -e /dev/kmsg ] || ln -sf /dev/console /dev/kmsg'" \
      > /etc/systemd/system/kubelet.service.d/10-kmsg.conf
