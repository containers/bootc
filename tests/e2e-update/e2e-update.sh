#!/bin/bash
# Given a coreos-assembler dir (COSA_DIR) and assuming
# the current dir is a git repository for bootupd,
# synthesize a test update and upgrade to it.  This
# assumes that the latest cosa build is using the
# code we want to test (as happens in CI).
set -euo pipefail

dn=$(cd $(dirname $0) && pwd)
testprefix=$(cd ${dn} && git rev-parse --show-prefix)
. ${dn}/../kola/data/libtest.sh
. ${dn}/testrpmbuild.sh

if test -z "${COSA_DIR:-}"; then
    fatal "COSA_DIR must be set"
fi
# Validate source directory
bootupd_git=$(cd ${dn} && git rev-parse --show-toplevel)
# https://github.com/coreos/bootupd/issues/551
! test -f ${bootupd_git}/systemd/bootupd.service

testtmp=$(mktemp -d -p /var/tmp bootupd-e2e.XXXXXXX)
export test_tmpdir=${testtmp}

# This is new content for our update
test_bootupd_payload_file=/boot/efi/EFI/fedora/test-bootupd.efi
test_bootupd_payload_file1=/boot/efi/EFI/BOOT/test-bootupd1.efi
build_rpm test-bootupd-payload \
  files "${test_bootupd_payload_file}
         ${test_bootupd_payload_file1}" \
  install "mkdir -p %{buildroot}/$(dirname ${test_bootupd_payload_file})
           echo test-payload > %{buildroot}/${test_bootupd_payload_file}
           mkdir -p %{buildroot}/$(dirname ${test_bootupd_payload_file1})
           echo test-payload1 > %{buildroot}/${test_bootupd_payload_file1}"

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

create_manifest_fork() {
    if test ! -f src/config/bootupd-fork; then
        echo "NOTICE: overriding src/config in ${COSA_DIR}"
        sleep 2
        runv rm -rf src/config.bootupd-testing-old
        runv mv src/config src/config.orig
        runv git clone src/config.orig src/config
        touch src/config/bootupd-fork
        # This will fall over if the upstream manifest gains `packages:`
        cat >> src/config/manifest.yaml << EOF
packages:
  - test-bootupd-payload
EOF
        echo "forked src/config"
    else
        fatal "already forked manifest"
    fi
}

undo_manifest_fork() {
    test -d src/config.orig
    assert_file_has_content src/config/manifest.yaml test-bootupd-payload
    if test -f src/config/bootupd-fork; then
        runv rm src/config -rf
    else
        # Keep this around just in case
        runv mv src/config{,.bootupd-testing-old}
    fi
    runv mv src/config.orig src/config
    test ! -f src/config/bootupd-fork
    echo "undo src/config fork OK"
}

if test -z "${e2e_skip_build:-}"; then
    echo "Building starting image"
    rm -f ${overrides}/rpm/*.rpm
    # Version from F39 GA
    add_override grub2-2.06-100.fc39
    runv cosa build
    prev_image=$(runv cosa meta --image-path qemu)
    create_manifest_fork
    rm -f ${overrides}/rpm/*.rpm
    echo "Building update ostree"
    # Version queued in current updates
    add_override grub2-2.06-123.fc40
    mv ${test_tmpdir}/yumrepo/packages/$(arch)/*.rpm ${overrides}/rpm/
    # Only build ostree update
    runv cosa build ostree
    undo_manifest_fork
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

cat >${testtmp}/test.bu << EOF
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
        ExecStart=/bin/sh -c '/run/bootupd-source/${testprefix}/e2e-update-in-vm.sh &>>/run/testtmp/out.txt; test -f /run/rebooting || poweroff -ff'
        [Install]
        WantedBy=multi-user.target
EOF
runv butane -o ${testtmp}/test.ign ${testtmp}/test.bu
cd ${testtmp}
qemuexec_args=(kola qemuexec --propagate-initramfs-failure --qemu-image "${prev_image}" --qemu-firmware uefi \
    -i test.ign --bind-ro ${COSA_DIR},/run/cosadir --bind-ro ${bootupd_git},/run/bootupd-source --bind-rw ${testtmp},/run/testtmp)
if test -n "${e2e_debug:-}"; then
    runv ${qemuexec_args[@]} --devshell
else
    runv timeout 5m "${qemuexec_args[@]}" --console-to-file ${COSA_DIR}/tmp/console.txt
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
