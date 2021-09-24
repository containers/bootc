#!/bin/bash
set -xeuo pipefail

yum -y install skopeo
yum -y --enablerepo=updates-testing update ostree-devel

git clone --depth=1 https://github.com/cgwalters/container-image-proxy
cd container-image-proxy
make
install -m 0755 bin/container-image-proxy /usr/bin/
