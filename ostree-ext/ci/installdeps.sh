#!/bin/bash
set -xeuo pipefail

# Always pull ostree from updates-testing to avoid the bodhi wait
dnf -y --enablerepo=updates-testing update ostree-devel

# Pull the code from https://github.com/containers/skopeo/pull/1476
# if necessary.
if ! skopeo experimental-image-proxy --help &>/dev/null; then
    dnf -y install dnf-utils
    dnf builddep -y skopeo
    git clone --depth=1 https://github.com/containers/skopeo
    cd skopeo
    make
    install -m 0755 bin/skopeo /usr/bin/
fi

