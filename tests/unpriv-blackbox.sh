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
    echo "Skipping cleanup of ${test_tmpdir}"
  fi
}

bootupd() {
    runv ${dn}/../target/release/bootupd "$@"
}

bootefi=root/boot/efi
ostefi=root/usr/lib/ostree-boot/efi
mkdir -p "${bootefi}" "${ostefi}"
for dir in "${bootefi}" "${ostefi}"; do
  (cd ${dir}
   mkdir -p EFI/fedora
   cd EFI/fedora
   for x in grubx64.efi shim.efi shimx64.efi; do
    echo "some code for ${x}" > ${x}
   done
  )
done

v0_digest="sha512:4vbqyRvLYPyPtVaD3VexZQ9FBzhh1MFNsK4A5t1Ju7bVvj3YEDZfrrBQ7CTNjgwj8PhRBPvxHJ5v3x28fQZxCKBL"

validate_v0() {
  bootupd status --sysroot=root --component=EFI | tee out.txt
  assert_file_has_content_literal out.txt 'Component EFI'
  assert_file_has_content_literal out.txt '  Unmanaged: digest='${v0_digest}
  assert_file_has_content_literal out.txt 'Update: At latest version'
  assert_not_file_has_content out.txt 'Component BIOS'
}

# This hack avoids us depending on having an ostree sysroot set up for now
export BOOT_UPDATE_TEST_TIMESTAMP=$(date -u --iso-8601=seconds)
validate_v0

echo 'v2 code for grubx64.efi' > "${ostefi}"/EFI/fedora/grubx64.efi
bootupd status --sysroot=root --component=EFI | tee out.txt
assert_file_has_content_literal out.txt 'Update: Available: '${BOOT_UPDATE_TEST_TIMESTAMP}
assert_file_has_content_literal out.txt '    Diff: changed=1 added=0 removed=0'

# Revert back
echo 'some code for grubx64.efi' > "${ostefi}"/EFI/fedora/grubx64.efi
validate_v0

bootupd adopt --sysroot=root | tee out.txt
assert_file_has_content_literal out.txt "Adopting: EFI"
ok 'rerunning adoption'

bootupd adopt --sysroot=root | tee out.txt
assert_not_file_has_content_literal out.txt "Adopting: EFI"
assert_file_has_content_literal out.txt "Nothing to do"
ok '"rerunning adopt is idempotent'

bootupd status --sysroot=root --component=EFI | tee out.txt
assert_file_has_content_literal out.txt 'Component EFI'
assert_file_has_content_literal out.txt '  Installed: '${v0_digest}
assert_file_has_content_literal out.txt '  Adopted: true'
assert_file_has_content_literal out.txt 'Update: At latest version'
ok 'adoption status'

tap_finish
