#!/bin/bash
# Given an old FCOS build (pre-bootupd), upgrade
# to the latest build in ${COSA_DIR} and run through
# the adoption procedure to update the ESP.
set -euo pipefail

# There was a grub2-efi-x64 change after this
PRE_BOOTUPD_FCOS=https://builds.coreos.fedoraproject.org/prod/streams/stable/builds/32.20200907.3.0/x86_64/meta.json

dn=$(cd $(dirname $0) && pwd)
testprefix=$(cd ${dn} && git rev-parse --show-prefix)
. ${dn}/../kola/data/libtest.sh

if test -z "${COSA_DIR:-}"; then
    fatal "COSA_DIR must be set"
fi
# Validate source directory
bootupd_git=$(cd ${dn} && git rev-parse --show-toplevel)
test -f ${bootupd_git}/systemd/bootupd.service

testtmp=$(mktemp -d -p /var/tmp bootupd-e2e.XXXXXXX)
export test_tmpdir=${testtmp}
cd ${test_tmpdir}
runv curl -sSL -o meta.json ${PRE_BOOTUPD_FCOS}
jq .images.qemu < meta.json > qemu.json
qemu_image_xz=$(jq -r .path < qemu.json)
qemu_image=${qemu_image_xz%%.xz}
if test -f "${COSA_DIR}/tmp/${qemu_image}"; then
    qemu_image="${COSA_DIR}/tmp/${qemu_image}"
else
    runv curl -sSL $(dirname ${PRE_BOOTUPD_FCOS})/${qemu_image_xz} | xz -d > ${COSA_DIR}/tmp/${qemu_image}.tmp
    mv ${COSA_DIR}/tmp/${qemu_image}{.tmp,}
    qemu_image=${COSA_DIR}/tmp/${qemu_image}
fi

# Start in cosa dir
cd ${COSA_DIR}
test -d builds

echo "Preparing test"
target_commit=$(cosa meta --get-value ostree-commit)
echo "Target commit: ${target_commit}"

execstop='test -f /run/rebooting || poweroff -ff'
if test -n "${e2e_debug:-}"; then
    execstop=
fi
cat >${testtmp}/test.fcct << EOF
variant: fcos
version: 1.0.0
systemd:
  units:
    - name: zincati.service
      dropins:
        - name: disabled.conf
          contents: |
            [Unit]
            # Disable zincati, we're going to do our own updates
            ConditionPathExists=/nosuchfile
    - name: bootupd-test.service
      enabled: true
      contents: |
        [Unit]
        RequiresMountsFor=/run/testtmp
        [Service]
        Type=oneshot
        RemainAfterExit=yes
        Environment=TARGET_COMMIT=${target_commit}
        Environment=SRCDIR=/run/bootupd-source
        # Run via shell because selinux denies systemd writing to 9p apparently
        ExecStart=/bin/sh -c '/run/bootupd-source/${testprefix}/e2e-adopt-in-vm.sh &>>/run/testtmp/out.txt; ${execstop}'
        [Install]
        WantedBy=multi-user.target
EOF
runv fcct -o ${testtmp}/test.ign ${testtmp}/test.fcct
cd ${testtmp}
qemuexec_args=(kola qemuexec --propagate-initramfs-failure --qemu-image "${qemu_image}" --qemu-firmware uefi \
    -i test.ign --bind-ro ${COSA_DIR},/run/cosadir --bind-ro ${bootupd_git},/run/bootupd-source --bind-rw .,/run/testtmp)
if test -n "${e2e_debug:-}"; then
    runv ${qemuexec_args[@]} --devshell
else
    runv timeout 5m "${qemuexec_args[@]}" --console-to-file $(pwd)/console.txt
fi
if ! test -f ${testtmp}/success; then
    if test -s ${testtmp}/out.txt; then
        sed -e 's,^,# ,' < ${testtmp}/out.txt
    else
        echo "No out.txt created, systemd unit failed to start"
    fi
    fatal "test failed"
fi
echo "ok bootupd e2e"
