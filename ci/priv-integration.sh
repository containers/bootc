#!/bin/bash
# Assumes that the current environment is a privileged container
# with the host mounted at /run/host.  We can basically write
# whatever we want, however we can't actually *reboot* the host.
set -euo pipefail

# https://github.com/ostreedev/ostree-rs-ext/issues/417
mkdir -p /var/tmp

sysroot=/run/host
# Current stable image fixture
image=quay.io/fedora/fedora-coreos:testing-devel
old_image=quay.io/cgwalters/fcos:unchunked
imgref=ostree-unverified-registry:${image}
stateroot=testos

set -x

if test '!' -e "${sysroot}/ostree"; then
    ostree admin init-fs --modern "${sysroot}"
    ostree config --repo $sysroot/ostree/repo set sysroot.bootloader none
fi
if test '!' -d "${sysroot}/ostree/deploy/${stateroot}"; then
    ostree admin os-init "${stateroot}" --sysroot "${sysroot}"
fi
# Test the syntax which uses full imgrefs.
ostree-ext-cli container image deploy --sysroot "${sysroot}" \
    --stateroot "${stateroot}" --imgref "${imgref}"
ostree admin --sysroot="${sysroot}" status
ostree-ext-cli container image remove --repo "${sysroot}/ostree/repo" registry:"${image}"
ostree admin --sysroot="${sysroot}" undeploy 0
# Now test the new syntax which has a nicer --image that defaults to registry.
ostree-ext-cli container image deploy --transport registry --sysroot "${sysroot}" \
    --stateroot "${stateroot}" --image "${image}" --no-signature-verification
ostree admin --sysroot="${sysroot}" status
ostree-ext-cli container image remove --repo "${sysroot}/ostree/repo" registry:"${image}"
ostree admin --sysroot="${sysroot}" undeploy 0

for img in "${image}"; do
    ostree-ext-cli container image deploy --sysroot "${sysroot}" \
        --stateroot "${stateroot}" --imgref ostree-unverified-registry:"${img}"
    ostree admin --sysroot="${sysroot}" status
    initial_refs=$(ostree --repo="${sysroot}/ostree/repo" refs | wc -l)
    ostree-ext-cli container image remove --repo "${sysroot}/ostree/repo" registry:"${img}"
    pruned_refs=$(ostree --repo="${sysroot}/ostree/repo" refs | wc -l)
    # Removing the image should only drop the image reference, not its layers
    test "$(($initial_refs - 1))" = "$pruned_refs"
    ostree admin --sysroot="${sysroot}" undeploy 0
    # TODO: when we fold together ostree and ostree-ext, automatically prune layers
    ostree-ext-cli container image prune-layers --repo="${sysroot}/ostree/repo"
    ostree --repo="${sysroot}/ostree/repo" refs > refs.txt
    if test "$(wc -l < refs.txt)" -ne 0; then
        echo "found refs"
        cat refs.txt
        exit 1
    fi
done

if ostree-ext-cli container image deploy --sysroot "${sysroot}" \
        --stateroot "${stateroot}" --imgref ostree-unverified-registry:"${old_image}" 2>err.txt; then
    echo "deployed old image"
    exit 1
fi
grep 'legacy format.*no longer supported' err.txt
echo "ok old image failed to parse"

# Verify we have systemd journal messages
nsenter -m -t 1 journalctl _COMM=ostree-ext-cli > logs.txt
grep 'layers already present: ' logs.txt

podman pull ${image}
ostree --repo="${sysroot}/ostree/repo" init --mode=bare-user
ostree-ext-cli container image pull ${sysroot}/ostree/repo ostree-unverified-image:containers-storage:${image}
echo "ok pulled from containers storage"

ostree-ext-cli container compare ${imgref} ${imgref} > compare.txt
grep "Removed layers: 0  Size: 0 bytes" compare.txt
grep "Added layers: 0  Size: 0 bytes" compare.txt

echo ok privileged integration
