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
imgref=ostree-unverified-registry:${image}
stateroot=testos

cd $(mktemp -d -p /var/tmp)

set -x

if test '!' -e "${sysroot}/ostree"; then
    ostree admin init-fs --modern "${sysroot}"
    ostree config --repo $sysroot/ostree/repo set sysroot.bootloader none
fi
if test '!' -d "${sysroot}/ostree/deploy/${stateroot}"; then
    ostree admin os-init "${stateroot}" --sysroot "${sysroot}"
fi
# Should be no images pruned
ostree-ext-cli container image prune-images --sysroot "${sysroot}"
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
ostree admin --sysroot="${sysroot}" undeploy 0
# Now we should prune it
ostree-ext-cli container image prune-images --sysroot "${sysroot}"
ostree-ext-cli container image list --repo "${sysroot}/ostree/repo" > out.txt
test $(stat -c '%s' out.txt) = 0

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

# Verify we have systemd journal messages
nsenter -m -t 1 journalctl _COMM=ostree-ext-cli > logs.txt
grep 'layers already present: ' logs.txt

podman pull ${image}
ostree --repo="${sysroot}/ostree/repo" init --mode=bare-user
ostree-ext-cli container image pull ${sysroot}/ostree/repo ostree-unverified-image:containers-storage:${image}
echo "ok pulled from containers storage"

ostree-ext-cli container compare ${imgref} ${imgref} > compare.txt
grep "Removed layers: *0 *Size: 0 bytes" compare.txt
grep "Added layers: *0 *Size: 0 bytes" compare.txt

mkdir build
cd build
cat >Dockerfile << EOF
FROM ${image}
RUN touch /usr/share/somefile
EOF
systemd-run -dP --wait podman build -t localhost/fcos-derived .
derived_img=oci:/var/tmp/derived.oci
systemd-run -dP --wait skopeo copy containers-storage:localhost/fcos-derived "${derived_img}"

# Prune to reset state
ostree refs ostree/container/image --delete

repo="${sysroot}/ostree/repo"
images=$(ostree container image list --repo "${repo}" | wc -l)
test "${images}" -eq 1
ostree-ext-cli container image deploy --sysroot "${sysroot}" \
        --stateroot "${stateroot}" --imgref ostree-unverified-image:"${derived_img}"
imgref=$(ostree refs --repo=${repo} ostree/container/image | head -1)
img_commit=$(ostree --repo=${repo} rev-parse ostree/container/image/${imgref})
ostree-ext-cli container image remove --repo "${repo}" "${derived_img}"

ostree-ext-cli container image deploy --sysroot "${sysroot}" \
        --stateroot "${stateroot}" --imgref ostree-unverified-image:"${derived_img}"
img_commit2=$(ostree --repo=${repo} rev-parse ostree/container/image/${imgref})
test "${img_commit}" = "${img_commit2}"
echo "ok deploy derived container identical revs"

echo ok privileged integration
