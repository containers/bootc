#!/bin/bash
# Verify `ostree container commit`
set -euo pipefail

image=quay.io/coreos-assembler/fcos:stable
example=coreos-layering-examples/tailscale
set -x

chmod a+x ostree-ext-cli
workdir=${PWD}
cd ${example}
cp ${workdir}/ostree-ext-cli .
sed -ie 's,ostree container commit,ostree-ext-cli container commit,' Dockerfile
sed -ie 's,^\(FROM .*\),\1\nADD ostree-ext-cli /usr/bin/,' Dockerfile
git diff

for runtime in podman docker; do
    $runtime build -t localhost/fcos-tailscale .
    $runtime run --rm localhost/fcos-tailscale rpm -q tailscale
done

cd $(mktemp -d)
cp ${workdir}/ostree-ext-cli .
cat > Dockerfile << EOF
FROM $image
ADD ostree-ext-cli /usr/bin/
RUN set -x; test \$(ostree-ext-cli internal-only-for-testing detect-env) = ostree-container
EOF
# Also verify docker buildx, which apparently doesn't have /.dockerenv
docker buildx build -t localhost/fcos-tailscale .

echo ok container image integration
