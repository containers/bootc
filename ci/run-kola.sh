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

# Detect Prow; if we find it, assume the image requires a pull secret
kola_args=()
if test -n "${JOB_NAME_HASH:-}"; then
    oc registry login --to auth.json
    cat > pull-secret.bu << 'EOF'
variant: fcos
version: 1.1.0
storage:
  files:
    - path: /etc/ostree/auth.json
      contents:
        local: auth.json
systemd:
  units:
    - name: zincati.service
      dropins:
        - name: disabled.conf
          contents: |
            [Unit]
            ConditionPathExists=/enoent

EOF
    butane -d . < pull-secret.bu > pull-secret.ign
    kola_args+=("--append-ignition" "pull-secret.ign")
fi

if test -z "${BASE_QEMU_IMAGE:-}"; then
    coreos-installer download -p qemu -f qcow2.xz --decompress
    BASE_QEMU_IMAGE="$(echo *.qcow2)"
fi
cosa kola run --oscontainer ostree-unverified-registry:${TARGET_IMAGE} --qemu-image "./${BASE_QEMU_IMAGE}" "${kola_args[@]}" ext.bootc.'*'

echo "ok kola bootc"
