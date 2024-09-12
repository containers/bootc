#!/bin/bash
# Run inside the vm spawned from e2e.sh
set -euo pipefail

dn=$(cd $(dirname $0) && pwd)
bn=$(basename $0)
. ${dn}/../kola/data/libtest.sh

cd $(mktemp -d)

echo "Starting $0"

current_commit=$(rpm-ostree status --json | jq -r .deployments[0].checksum)

stampfile=/etc/${bn}.upgraded
if ! test -f ${stampfile}; then
    if test "${current_commit}" = ${TARGET_COMMIT}; then
        fatal "already at ${TARGET_COMMIT}"
    fi

    current_grub=$(rpm -q --queryformat='%{nevra}\n' ${TARGET_GRUB_NAME})
    if test "${current_grub}" == "${TARGET_GRUB_PKG}"; then
        fatal "Current grub ${current_grub} is same as target ${TARGET_GRUB_PKG}"
    fi

    # FIXME
    # https://github.com/coreos/rpm-ostree/issues/2210
    runv setenforce 0
    runv rpm-ostree rebase /run/cosadir/tmp/repo:${TARGET_COMMIT}
    runv touch ${stampfile}
    runv systemd-run -- systemctl reboot
    touch /run/rebooting
    sleep infinity
else
    if test "${current_commit}" != ${TARGET_COMMIT}; then
        fatal "not at ${TARGET_COMMIT}"
    fi
fi

# We did setenforce 0 above for https://github.com/coreos/rpm-ostree/issues/2210
# Validate that on reboot we're still enforcing.
semode=$(getenforce)
if test "$semode" != Enforcing; then
    fatal "SELinux mode is ${semode}"
fi

if ! test -n "${TARGET_GRUB_PKG}"; then
    fatal "Missing TARGET_GRUB_PKG"
fi

bootupctl validate
ok validate

bootupctl status | tee out.txt
assert_file_has_content_literal out.txt 'Component EFI'
assert_file_has_content_literal out.txt '  Installed: grub2-efi-x64-'
assert_not_file_has_content out.txt '  Installed:.*test-bootupd-payload'
assert_not_file_has_content out.txt '  Installed:.*'"${TARGET_GRUB_PKG}"
assert_file_has_content out.txt 'Update: Available:.*'"${TARGET_GRUB_PKG}"
assert_file_has_content out.txt 'Update: Available:.*test-bootupd-payload-1.0'
bootupctl status --print-if-available > out.txt
assert_file_has_content_literal 'out.txt' 'Updates available: BIOS EFI'
ok update avail

# Mount the EFI partition.
tmpefimount=$(mount_tmp_efi)

assert_not_has_file ${tmpefimount}/EFI/fedora/test-bootupd.efi

if env FAILPOINTS='update::exchange=return' bootupctl update -vvv 2>err.txt; then
    fatal "should have errored"
fi
assert_file_has_content err.txt "error: .*synthetic failpoint"

bootupctl update -vvv | tee out.txt
assert_file_has_content out.txt "Previous EFI: .*"
assert_file_has_content out.txt "Updated EFI: ${TARGET_GRUB_PKG}.*,test-bootupd-payload-1.0"

assert_file_has_content ${tmpefimount}/EFI/fedora/test-bootupd.efi test-payload

bootupctl status --print-if-available > out.txt
if test -s out.txt; then
    fatal "Found available updates: $(cat out.txt)"
fi
ok update not avail

mount -o remount,rw /boot
rm -f /boot/bootupd-state.json
bootupctl adopt-and-update | tee out.txt
assert_file_has_content out.txt "Adopted and updated: BIOS: .*"
assert_file_has_content out.txt "Adopted and updated: EFI: .*"
ok adopt-and-update

tap_finish
touch /run/testtmp/success
sync
# TODO maybe try to make this use more of the exttest infrastructure?
exec poweroff -ff
