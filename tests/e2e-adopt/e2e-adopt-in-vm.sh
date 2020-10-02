#!/bin/bash
# Run inside the vm spawned from e2e.sh
set -euo pipefail

dn=$(cd $(dirname $0) && pwd)
bn=$(basename $0)
. ${dn}/../kola/data/libtest.sh

cd $(mktemp -d)

echo "Starting $0"

enable_bootupd() {
    systemctl start bootupd.socket
    # For now
    export BOOTUPD_ACCEPT_PREVIEW=1
}

current_commit=$(rpm-ostree status --json | jq -r .deployments[0].checksum)

reboot_with_mark() {
    mark=$1; shift
    runv echo ${mark} > ${reboot_mark_path}
    sync ${reboot_mark_path}
    runv systemd-run -- systemctl reboot
    touch /run/rebooting
    sleep infinity
}

status_ok_no_update() {
    bootupctl status | tee out.txt
    assert_file_has_content_literal out.txt 'Component EFI'
    assert_file_has_content_literal out.txt '  Installed: grub2-efi-x64-'
    assert_file_has_content_literal out.txt 'Update: At latest version'
    assert_file_has_content out.txt 'CoreOS aleph image ID: .*coreos.*-qemu'
    bootupctl validate
    ok status and validate
}

reboot_mark_path=/etc/${bn}.rebootstamp
reboot_mark=
if test -f "${reboot_mark_path}"; then
    reboot_mark=$(cat ${reboot_mark_path})
fi
case "${reboot_mark}" in
    "") 
    if test "${current_commit}" = ${TARGET_COMMIT}; then
        fatal "already at ${TARGET_COMMIT}"
    fi

    # This system wasn't built via bootupd
    assert_not_has_file /boot/bootupd-state.json

    # FIXME
    # https://github.com/coreos/rpm-ostree/issues/2210
    runv setenforce 0
    runv rpm-ostree rebase /run/cosadir/tmp/repo:${TARGET_COMMIT}
    reboot_with_mark first
    ;;
    first)
    if test "${current_commit}" != ${TARGET_COMMIT}; then
        fatal "not at ${TARGET_COMMIT}"
    fi
    # NOTE Fall through NOTE
    ;;
    second)
    enable_bootupd
    status_ok_no_update
    touch /run/testtmp/success
    sync
    # TODO maybe try to make this use more of the exttest infrastructure?
    exec poweroff -ff
    ;;
esac

enable_bootupd

# We did setenforce 0 above for https://github.com/coreos/rpm-ostree/issues/2210
# Validate that on reboot we're still enforcing.
semode=$(getenforce)
if test "$semode" != Enforcing; then
    fatal "SELinux mode is ${semode}"
fi

source_grub_cfg=$(find /boot/efi/EFI -name grub.cfg)
test -f "${source_grub_cfg}"

source_grub=$(find /boot/efi/EFI -name grubx64.efi)
test -f ${source_grub}
source_grub_sha256=$(sha256sum ${source_grub} | cut -f 1 -d ' ')

update_grub=$(find /usr/lib/bootupd/updates/EFI/ -name grubx64.efi)
test -f ${update_grub}
update_grub_sha256=$(sha256sum ${update_grub} | cut -f 1 -d ' ')
if test "${source_grub_sha256}" = "${update_grub_sha256}"; then
    fatal "Already have target grubx64.efi"
fi

bootupctl status | tee out.txt
assert_file_has_content_literal out.txt 'No components installed.'
assert_file_has_content out.txt 'Adoptable: EFI: .*coreos.*-qemu.*'

bootupctl validate | tee out.txt
assert_file_has_content_literal out.txt 'No components installed.'
assert_not_file_has_content_literal out.txt "Validated"
# Shouldn't write state just starting and validating
assert_not_has_file /boot/bootupd-state.json
ok validate

bootupctl adopt-and-update | tee out.txt
assert_file_has_content out.txt 'Adopted and updated: EFI: grub2-efi-x64'
ok adoption

status_ok_no_update

bootupctl validate | tee out.txt
assert_not_file_has_content_literal out.txt "Validated EFI"

new_grub_sha256=$(sha256sum ${source_grub} | cut -f 1 -d ' ')
if test "${new_grub_sha256}" != "${update_grub_sha256}"; then
    fatal "Failed to update grub"
fi
ok updated grub

# We shouldn't have deleted the config file which was unmanaged
test -f "${source_grub_cfg}"
ok still have grub.cfg

tap_finish

# And now do another reboot to validate that things are good
reboot_with_mark second

