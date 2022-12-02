#!/bin/bash
set -xeuo pipefail

# We require the an image containing bootc-under-test to have been injected
# by an external system, e.g. Prow 
# https://docs.ci.openshift.org/docs/architecture/ci-operator/#referring-to-images-in-tests
if test -z "${TARGET_IMAGE:-}"; then
    echo "fatal: Must set TARGET_IMAGE" 1>&2; exit 1
fi
echo "Test base image: ${TARGET_IMAGE}"

tmpdir="$(mktemp -d -p /var/tmp)"
cd "${tmpdir}"
if test -z "${BASE_QEMU_IMAGE:-}"; then
    coreos-installer download -p qemu -f qcow2.xz --decompress
    BASE_QEMU_IMAGE="$(echo *.qcow2)"
fi
kola run --oscontainer ostree-unverified-registry:${TARGET_IMAGE} --qemu-image "./${BASE_QEMU_IMAGE}" ext.bootc.'*'

echo "ok kola bootc"
