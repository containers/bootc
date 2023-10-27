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

 * Fedora: [bootc is packaged](https://bodhi.fedoraproject.org/updates/?packages=bootc) and be available in the main repositories soon.
 * CentOS Stream 9: There is a [COPR](https://copr.fedorainfracloud.org/coprs/rhcontainerbot/bootc/) tracking git main with binary packages.

You can also build this project like any other Rust project, e.g. `cargo build --release` from a git clone.

### Base images

Many users will be more interested in base (container) images.

To build base images "from scratch", see [docs/bootc-images.md].

For pre-built base images:

* [Fedora CoreOS](https://quay.io/repository/fedora/fedora-coreos) can be used as a base image; you will need to [enable bootc](https://github.com/coreos/rpm-ostree/blob/main/docs/bootc.md) there.
* There is also an in-development [Project Sagano](https://gitlab.com/CentOS/cloud/sagano) for Fedora/CentOS.

However, bootc itself is not tied to Fedora derivatives; [this issue](https://github.com/coreos/bootupd/issues/468) tracks the main blocker for other distributions.

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

### Relationship with podman

It gets a bit confusing to talk about shipping bootable operating systems in container images.
Again, to be clear: we are reusing container images as:

- A build mechanism (including running *as* a standard OCI container image)
- A transport mechanism

But, actually when a bootc container is booted, podman (or docker, etc.) is not involved.
The storage used for the operating system content is distinct from `/var/lib/containers`.
`podman image prune --all` will not delete your operating system.

That said, a toplevel goal of bootc is alignment with the https://github.com/containers ecosystem,
which includes podman.  But more specifically at a technical level, today bootc uses
[skopeo](https://github.com/containers/skopeo/) and hence indirectly [containers/image](https://github.com/containers/image)
as a way to fetch container images.

This means that bootc automatically also honors many of the knobs available in `/etc/containers` - specifically
things like [containers-registries.conf](https://github.com/containers/image/blob/main/docs/containers-registries.conf.5.md).

In other words, if you configure `podman` to pull images from your local mirror registry, then `bootc` will automatically honor that as well.

The simple way to say it is: A goal of `bootc` is to be the bootable-container analogue for `podman`, which runs application containers.  Everywhere one might run `podman`, one could also consider using `bootc`. 

### Relationship with Kubernetes

Just as `podman` does not depend on a Kubernetes API server, `bootc` will also not depend on one.

However, there are also plans for `bootc` to also understand Kubernetes API types.  See [configmap/secret support](https://github.com/containers/bootc/issues/22) for example.

Perhaps in the future we may actually support some kind of `Pod` analogue for representing the host state.  Or we may define a [CRD](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/) which can be used inside and outside of Kubernetes.

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

### Relationship with Fedora CoreOS (and Silverblue, etc.)

Per above, it is a toplevel goal to support a seamless, transactional update from existing OSTree based systems, which includes these Fedora derivatives.

For Fedora CoreOS specifically, see [this tracker issue](https://github.com/coreos/fedora-coreos-tracker/issues/1446).

See also [OstreeNativeContainerStable](https://fedoraproject.org/wiki/Changes/OstreeNativeContainerStable).

# More links

- https://coreos.github.io/rpm-ostree/container/
- https://github.com/coreos/layering-examples

