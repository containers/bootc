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

## ostree

This project currently leverages significant work done in
[the ostree project](https://github.com/ostreedev/ostree-rs-ext/).

In the future, there may be non-ostree backends.

## Modeling operating system hosts as containers 

The bootc project suggests that Linux operating systems and distributions
to provide a new kind of "bootable" base image, distinct from "application"
base images.  A reference example available today is
[Fedora CoreOS](https://quay.io/repository/fedora/fedora-coreos).

## Deriving from and switching to base images

A toplevel goal is that *every tool and technique* a Linux system
administrator knows around how to build, inspect, mirror and manage
application containers also applies to bootable host systems.

There are a number of examples in e.g. [coreos/layering-examples](https://github.com/coreos/layering-examples).

First, build a derived container using any container build tooling.

Next, given a disk image (e.g. AMI, qcow2, raw disk image) installed on a host
system and set up using ostree by default, the `bootc switch` command
can be used to switch the system to use the targeted container image:

```
$ bootc switch --no-signature-verification quay.io/examplecorp/custom:latest
```

This will preserve existing state in `/etc` and `/var` - for example,
host SSH keys and home directories.

## Upgrading

Once a chosen container image is used as the boot source, further
invocations of `bootc upgrade` will look for newer versions - again
preserving state.

# More links 

- https://fedoraproject.org/wiki/Changes/OstreeNativeContainerStable
- https://github.com/coreos/layering-examples

