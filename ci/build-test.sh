#!/bin/bash
set -xeuo pipefail
test -n "${COSA_DIR:-}"
make
cosa build-fast
kola run -E $(pwd) --qemu-image fastbuild-*-qemu.qcow2  --qemu-firmware uefi ext.bootupd.'*'
