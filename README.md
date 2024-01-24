# bootc

Transactional, in-place operating system updates using OCI/Docker container images.

# Motivation

The original Docker container model of using "layers" to model
applications has been extremely successful.  This project
aims to apply the same technique for bootable host systems - using
standard OCI/Docker containers as a transport and delivery format
for base operating system updates.

The container image includes a Linux kernel (in e.g. `/usr/lib/modules`),
which is used to boot.  At runtime on a target system, the base userspace is
*not* itself running in a container by default.  For example, assuming
systemd is in use, systemd acts as pid1 as usual - there's no "outer" process.

# Example

To try bootc today run:
```
$ truncate -s 10G test-disk.img
$ sudo losetup -Pf test-disk.img
$ LOOP=$(sudo losetup | grep test-disk.img | cut -f1 -d' ')
$ sudo podman run --rm --privileged --pid=host --security-opt label=type:unconfined_t quay.io/centos-bootc/fedora-bootc:eln bootc install to-disk --generic-machine "$LOOP"
$ qemu-system-x86_64 -m 1500 -snapshot -accel kvm -cpu host -bios /usr/share/OVMF/OVMF_CODE.fd ./test-disk.img
```


# More information

See the [project documentation](https://containers.github.io/bootc/).
