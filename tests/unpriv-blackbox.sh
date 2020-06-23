#!/bin/bash
#
# Copyright (C) 2020 Colin Walters <walters@verbum.org>
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
dn=$(cd $(dirname $0) && pwd)

. ${dn}/libtest.sh

tmpdir=$(mktemp -d)
cd ${tmpdir}
echo "using tmpdir: ${tmpdir}"
touch .testtmp
trap cleanup EXIT
function cleanup () {
  if test -z "${TEST_SKIP_CLEANUP:-}"; then
    if test -f "${tmpdir}"/.testtmp; then
      cd /
      rm "${tmpdir}" -rf
    fi
  else
    echo "Skipping cleanup of ${tmpdir}"
  fi
}

bootupd() {
    runv ${dn}/../target/release/bootupd "$@"
}

bootefi=root/boot/efi
ostefi=root/usr/lib/ostree-boot/efi
initefiroot() {
  d=$1
  shift
  echo "Initializing EFI root $d"
  rm "${d}" -rf
  mkdir -p "${d}"
  (cd ${d}
  mkdir -p EFI/fedora
  cd EFI/fedora
  echo unchanging > shouldnotchange.efi
  echo unchanging2 > shouldnotchange2.efi
  for x in grubx64.efi shim.efi shimx64.efi; do
    echo "some code for ${x}" > ${x}
  done
  )
}

v0_digest="sha512:2rLmU783U9VowCdmCDpBdvdQV7N751vhz3d3kMYCDS9FdufhUkCX5Gf66Yia1UcTwYAXHcudR7D5tmGUcTt5hqBZ"

validate_v0() {
  bootupd status --sysroot=root --component=EFI | tee out.txt
  assert_file_has_content_literal out.txt 'Component EFI'
  assert_file_has_content_literal out.txt '  Unmanaged: digest='${v0_digest}
  assert_not_file_has_content_literal out.txt 'Update: Available: '
  assert_not_file_has_content out.txt 'Component BIOS'
}

update_shipped() {
  local v
  v=$1
  shift
  for x in grubx64.efi shim.efi shimx64.efi; do
    test -f ${ostefi}/EFI/fedora/${x}
    echo "version ${v} code for ${x}" > ${ostefi}/EFI/fedora/${x}
  done
}

# This hack avoids us depending on having an ostree sysroot set up for now
export BOOT_UPDATE_TEST_TIMESTAMP=$(date -u --iso-8601=seconds)
initefiroot "${bootefi}"
initefiroot "${ostefi}"
validate_v0
ok 'first validate'

bootupd update --sysroot=root | tee out.txt
assert_file_has_content_literal out.txt 'Skipping component EFI which is found but not adopted'
ok 'no changes'

update_shipped 1
rm -v ${ostefi}/EFI/fedora/shim.efi
bootupd status --sysroot=root --component=EFI | tee out.txt
validate_v0
ok 'still no avail changes if unmanaged'

# Revert back
initefiroot "${ostefi}"
validate_v0
ok 'revert'

if bootupd update --sysroot=root EFI 2>err.txt; then
  fatal "performed an update without adopting"
fi
assert_file_has_content_literal err.txt 'Component EFI is not tracked and must be adopted before update'
ok cannot update without adoption

bootupd adopt --sysroot=root | tee out.txt
assert_file_has_content_literal out.txt "Adopting: EFI"
ok 'adoption'

bootupd adopt --sysroot=root | tee out.txt
assert_not_file_has_content_literal out.txt "Adopting: EFI"
assert_file_has_content_literal out.txt "Nothing to do"
ok 'rerunning adopt is idempotent'

bootupd status --sysroot=root --component=EFI | tee out.txt
assert_file_has_content_literal out.txt 'Component EFI'
assert_file_has_content_literal out.txt '  Installed: '${v0_digest}
assert_file_has_content_literal out.txt '  Adopted: true'
assert_file_has_content_literal out.txt 'Update: At latest version'
ok 'adoption status'

echo 'oops state drift' >> "${bootefi}"/EFI/fedora/shimx64.efi
bootupd status --sysroot=root --component=EFI | tee out.txt
assert_file_has_content_literal out.txt 'warning: drift detected'
assert_file_has_content_literal out.txt 'Recorded: '${v0_digest}
assert_file_has_content_literal out.txt 'Actual: sha512:5Dfb6bjpfgxMN1KDAmNFnbzcQxZidiCwdZHwgQrdTrUZvExHrMCKoEnQ9muTowVkW7t4QJHve1APpwa6dLi5WDKF'
ok 'drift detected'

# Re-initialize and adopt with extra state
rm -v root/boot/bootupd-state.json
initefiroot "${bootefi}"
initefiroot "${ostefi}"

v1_digest="sha512:5LjsojQNor5tntzx6KBFxVGL3LjYXGg7imGttnb194J8Zb1j4HVLDMjFiZUi777x6dyx8RFjZe9wpvkdUeLAUoyr"

echo 'unmanaged grub config' > "${bootefi}"/EFI/fedora/grub.cfg
bootupd adopt --sysroot=root | tee out.txt
assert_file_has_content_literal out.txt "Adopting: EFI"
export BOOT_UPDATE_TEST_TIMESTAMP=$(date -u --iso-8601=seconds)
ok adopt 2

bootupd status --sysroot=root --component=EFI | tee out.txt
assert_file_has_content_literal out.txt 'Component EFI'
assert_file_has_content_literal out.txt '  Installed: '${v1_digest}
assert_file_has_content_literal out.txt '  Adopted: true'
assert_file_has_content_literal out.txt 'Update: At latest version'
ok 'adoption status 2'

v2_digest="sha512:4SoabM7zw6x9CsY64u6G9RFbEocVEQfhCDtVKUJeTPgyCUHTfZPKQacJNw5B23LGpFeFKVqABGJVnSzuLNynFscy"


update_shipped t2
bootupd status --sysroot=root --component=EFI | tee out.txt
assert_file_has_content_literal out.txt 'Update: Available'
assert_file_has_content_literal out.txt '    Timestamp: '${BOOT_UPDATE_TEST_TIMESTAMP}
assert_file_has_content_literal out.txt '    Digest: '${v2_digest}
assert_file_has_content_literal out.txt '    Diff: changed=3 added=0 removed=1'
ok 'avail update v2'

bootupd update --sysroot=root EFI | tee out.txt
assert_not_file_has_content_literal out.txt 'warning: drift detected'
assert_file_has_content_literal out.txt 'EFI: Updated to digest='${v2_digest}
if ! test -f "${bootefi}/EFI/fedora/grub.cfg"; then
  fatal "missing unmanaged grub cfg"
fi
ok 'update v2'

bootupd status --sysroot=root --component=EFI | tee out.txt
assert_file_has_content_literal out.txt 'Component EFI'
assert_file_has_content_literal out.txt '  Installed: '${v2_digest}
assert_file_has_content_literal out.txt '  Adopted: true'
assert_file_has_content_literal out.txt 'Update: At latest version'
ok 'update v2 status'

tap_finish
