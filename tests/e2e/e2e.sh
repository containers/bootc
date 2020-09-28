#!/bin/bash
# Given a coreos-assembler dir (COSA_DIR) and assuming
# the current dir is a git repository for bootupd,
# synthesize a test update and upgrade to it.  This
# assumes that the latest cosa build is using the
# code we want to test (as happens in CI).
set -euo pipefail

dn=$(cd $(dirname $0) && pwd)
. ${dn}/../kola/data/libtest.sh
. ${dn}/testrpmbuild.sh

if test -z "${COSA_DIR:-}"; then
    fatal "COSA_DIR must be set"
fi
# Validate source directory
bootupd_git=$(cd ${dn} && git rev-parse --show-toplevel)
test -f ${bootupd_git}/systemd/bootupd.service

# Start in cosa dir
cd ${COSA_DIR}
test -d builds

overrides=${COSA_DIR}/overrides
test -d "${overrides}"
mkdir -p ${overrides}/rpm
add_override() {
    override=$1
    shift
    # This relies on "gold" grub not being pruned, and different from what's
    # in the latest fcos
    (cd ${overrides}/rpm && runv koji download-build --arch=noarch --arch=$(arch) ${override})
}
if test -z "${e2e_skip_build:-}"; then
    echo "Building starting image"
    rm -f ${overrides}/rpm/*.rpm
    add_override grub2-2.04-22.fc32
    (cd ${bootupd_git} && runv make && runv make install DESTDIR=${overrides}/rootfs)
    runv cosa build
    prev_image=$(runv cosa meta --image-path qemu)
    rm -f ${overrides}/rpm/*.rpm
    echo "Building update ostree"
    add_override grub2-2.04-23.fc32
    # Only build ostree update
    runv cosa build ostree
fi
echo "Preparing test"
grubarch=
case $(arch) in
    x86_64) grubarch=x64;;
    aarch64) grubarch=aa64;;
    *) fatal "Unhandled arch $(arch)";;
esac
target_grub_name=grub2-efi-${grubarch}
target_grub_pkg=$(rpm -qp --queryformat='%{nevra}\n' ${overrides}/rpm/${target_grub_name}-2*.rpm)
target_commit=$(cosa meta --get-value ostree-commit)
echo "Target commit: ${target_commit}"
# For some reason 9p can't write to tmpfs
testtmp=$(mktemp -d -p /var/tmp bootupd-e2e.XXXXXXX)
cat >${testtmp}/test.fcct << EOF
variant: fcos
version: 1.0.0
systemd:
  units:
    - name: bootupd-test.service
      enabled: true
      contents: |
        [Unit]
        RequiresMountsFor=/run/testtmp
        [Service]
        Type=oneshot
        RemainAfterExit=yes
        Environment=TARGET_COMMIT=${target_commit}
        Environment=TARGET_GRUB_NAME=${target_grub_name}
        Environment=TARGET_GRUB_PKG=${target_grub_pkg}
        Environment=SRCDIR=/run/bootupd-source
        # Run via shell because selinux denies systemd writing to 9p apparently
        ExecStart=/bin/sh -c '/run/bootupd-source/tests/e2e/e2e-in-vm.sh &>>/run/testtmp/out.txt; test -f /run/rebooting || poweroff -ff'
        [Install]
        WantedBy=multi-user.target
EOF
runv fcct -o ${testtmp}/test.ign ${testtmp}/test.fcct
cd ${testtmp}
qemuexec_args=(kola qemuexec --propagate-initramfs-failure --qemu-image "${prev_image}" --qemu-firmware uefi \
    -i test.ign --bind-ro ${COSA_DIR},/run/cosadir --bind-ro ${bootupd_git},/run/bootupd-source --bind-rw .,/run/testtmp)
if test -n "${e2e_debug:-}"; then
    runv ${qemuexec_args[@]} --devshell
else
    runv timeout 5m "${qemuexec_args[@]}" -- -chardev file,id=log,path=console.txt -serial chardev:log
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
