#!/bin/bash
# Verify `ostree container commit`
set -euo pipefail

image=quay.io/coreos-assembler/fcos:stable
example=coreos-layering-examples/tailscale
set -x

mv ostree-ext-cli ${example}
cd ${example}
chmod a+x ostree-ext-cli
sed -ie 's,ostree container commit,ostree-ext-cli container commit,' Dockerfile
sed -ie 's,^\(FROM .*\),\1\nADD ostree-ext-cli /usr/bin,' Dockerfile
git diff

docker build -t localhost/fcos-tailscale .

docker run --rm localhost/fcos-tailscale rpm -q tailscale

echo ok container image integration
