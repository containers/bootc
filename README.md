# bootc

Transactional, in-place operating system updates using OCI/Docker container images.

STATUS: Experimental, subject to change!

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
mounted, persistent volumes.  More on this in [the ostree docs](https://ostreedev.github.io/ostree/adapting-existing/#system-layout).

## Status

At the current time, bootc is in active development and is not quite
considered ready for production use.  The command line interface
*might* change.  There is not yet stable RPC API.

However, it heavily relies on a lot of underlying technologies which
are tested, and the goal is to stabilize everything sometime in
2023.

## Using bootc

### Installing

 * Fedora, CentOS Stream 9: There is a [COPR](https://copr.fedorainfracloud.org/coprs/rhcontainerbot/bootc/) tracking git main with binary packages.

You can also build this project like any other Rust project, e.g. `cargo build --release` from a git clone.

### Base images

Many users will be more interested in base (container) images.

* The [bootc-demo-base-images](https://github.com/cgwalters/bootc-demo-base-images) contains demonstration (relatively) small images that can be used as a starting point.
* [Fedora CoreOS](https://quay.io/repository/fedora/fedora-coreos) can also be used as a base image, but it does not currently include `bootc`.

However, bootc itself is not tied to Fedora derivatives; and the plan is to extend the set of images.

### Deriving from and switching to base images

A toplevel goal is that *every tool and technique* a Linux system
administrator knows around how to build, inspect, mirror and manage
application containers also applies to bootable host systems.

There are a number of examples in e.g. [coreos/layering-examples](https://github.com/coreos/layering-examples).

First, build a derived container using any container build tooling.

#### Using `bootc install`

The `bootc install` command will write the current container to a disk, and set it up for booting.
In brief, the idea is that every container image shipping `bootc` also comes with a simple
installer that can set a system up to boot from it.  Crucially, if you create a 
*derivative* container image from a stock OS container image, it also automatically supports `bootc install`.

For more information, please see [docs/install.md](docs/install.md).

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
invocations of `bootc upgrade` will look for newer versions - again
preserving state.

## Relationship with other projects

### Relationship with rpm-ostree

Today rpm-ostree directly links to `ostree-rs-ext`, and hence
gains all the same container functionality.  This will likely
continue.  For example, with rpm-ostree (or, perhaps re-framed as
"dnf image"), it will continue to work to e.g. `dnf install`
(i.e. `rpm-ostree install`) on the *client side* system.  However, `bootc upgrade` would
(should) then error out as it will not understand how to upgrade
the system.

rpm-ostree also has significant other features such as
`rpm-ostree kargs` etc.

Overall, rpm-ostree is used in several important projects
and will continue to be maintained for many years to come.

However, for use cases which want a "pure" image based model,
using `bootc` will be more appealing.  bootc also does not
e.g. drag in dependencies on `libdnf` and the RPM stack.

bootc also has the benefit of starting as a pure Rust project;
and while it [doesn't have an IPC mechanism today](https://github.com/containers/bootc/issues/4), the surface
of such an API will be significantly smaller.

Further, bootc does aim to [include some of the functionality of zincati](https://github.com/containers/bootc/issues/5).

But all this said: *It will be supported to use both bootc and rpm-ostree together*; they are not exclusive.
For example, `bootc status` at least will still function even if packages are layered.

# More links

- https://fedoraproject.org/wiki/Changes/OstreeNativeContainerStable
- https://coreos.github.io/rpm-ostree/container/
- https://github.com/coreos/layering-examples

