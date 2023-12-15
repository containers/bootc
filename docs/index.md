---
nav_order: 1
---

# bootc

Transactional, in-place operating system updates using OCI/Docker container images.

STATUS: Stable enough for dev/test by interested parties, but all interfaces are subject to change.

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
base images.  See below for available images.

Effectively, these images contain a Linux kernel - and while this kernel
is not used when the image is used via e.g. `podman|docker run`, it *is*
used when booted via `bootc`.

In the current defaults, `/etc` and `/var` both act a bit like
mounted, persistent volumes.
More on this in [the ostree docs](https://ostreedev.github.io/ostree/adapting-existing/#system-layout).

## Status

The core `bootc update` functionality is really just the same
technology which has shipped for some time in rpm-ostree so there
should be absolutely no worries about using it for OS updates.
A number of people do this today.

That said bootc is in active development and some parts
are subject to change, such as the command line interface and
the CRD-like API exposed via `bootc edit`.`

The `bootc install` functionality is also more experimental.

## Using bootc

### Installing

 * Fedora: [bootc is packaged](https://bodhi.fedoraproject.org/updates/?packages=bootc).
 * CentOS Stream 9: There is a [COPR](https://copr.fedorainfracloud.org/coprs/rhcontainerbot/bootc/) tracking git main with binary packages.

You can also build this project like any other Rust project, e.g. `cargo build --release` from a git clone.

### Base images

Many users will be more interested in base (container) images.

For pre-built base images:

* [Fedora CoreOS](https://quay.io/repository/fedora/fedora-coreos) can be used as a base image; you will need to [enable bootc](https://github.com/coreos/rpm-ostree/blob/main/docs/bootc.md) there.
* There is also an in-development [centos-boot](https://github.com/centos/centos-boot) project.

However, bootc itself is not tied to Fedora derivatives; [this issue](https://github.com/coreos/bootupd/issues/468) tracks the main blocker for other distributions.

To build base images "from scratch", see [bootc-images.md](bootc-images.md).

### Deriving from and switching to base images

A toplevel goal is that *every tool and technique* a Linux system
administrator knows around how to build, inspect, mirror and manage
application containers also applies to bootable host systems.

There are a number of examples in e.g. [coreos/layering-examples](https://github.com/coreos/layering-examples).

First, build a derived container using any container build tooling.

#### Using `bootc install`

The `bootc install` command has two high level sub-commands; `to-disk` and `to-filesystem`.

The `bootc install to-disk` handles basically everything in taking the current container
and writing it to a disk, and set it up for booting and future in-place upgrades.

In brief, the idea is that every container image shipping `bootc` also comes with a simple
installer that can set a system up to boot from it.  Crucially, if you create a
*derivative* container image from a stock OS container image, it also automatically
supports `bootc install`.

For more information, please see [install.md](install.md).

#### Switching from an existing ostree-based system

If you have [an operating system already using ostree](https://ostreedev.github.io/ostree/#operating-systems-and-distributions-using-ostree) then you can use `bootc switch`:

```
$ bootc switch --no-signature-verification quay.io/examplecorp/custom:latest
```

This will preserve existing state in `/etc` and `/var` - for example,
host SSH keys and home directories.  There may be some issues with uid/gid
drift in this scenario however.

### Upgrading

Once a chosen container image is used as the boot source, further
invocations of `bootc upgrade` from the installed operating system
will fetch updates from the container image registry.

This is backed today by ostree, implementing an A/B style upgrade system.
Changes to the base image are staged, and the running system is not
changed by default.

Use `bootc upgrade --apply` to apply updates; today this will always
reboot.

# More links

- [rpm-ostree container](https://coreos.github.io/rpm-ostree/container/)
- [centos-boot](https://github.com/centos/centos-boot)
- [coreos/layering-examples](https://github.com/coreos/layering-examples)

