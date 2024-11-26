#!/bin/bash

# connect to the VM using https://libvirt.org/nss.html

set -e

# build the container image
sudo podman build --build-arg "sshpubkey=$(cat ~/.ssh/id_rsa.pub)" -f Containerfile -t localhost/bootc-lldb .

# build the disk image
mkdir -p ~/.cache/bootc-dev/disks
rm -f ~/.cache/bootc-dev/disks/lldb.raw
truncate -s 10G ~/.cache/bootc-dev/disks/lldb.raw
sudo podman run --pid=host --network=host --privileged --security-opt label=type:unconfined_t -v ~/.cache/bootc-dev/disks:/output localhost/bootc-lldb bootc install to-disk --via-loopback --generic-image --skip-fetch-check /output/lldb.raw

# create a new VM in libvirt
set +e
virsh -c qemu:///system destroy bootc-lldb
virsh -c qemu:///system undefine --nvram bootc-lldb
set -e
sudo virt-install --name bootc-lldb --cpu host --vcpus 8 --memory 8192 --import --disk ~/.cache/bootc-dev/disks/lldb.raw --os-variant rhel9-unknown
