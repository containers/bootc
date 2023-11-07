---
nav_order: 2
---

# "bootc compatible" images

At the current time, it does not work to just do:
```
FROM fedora
RUN dnf -y install kernel
```
or
```
FROM debian
RUN apt install kernel
```

And get an image compatible with bootc.  Supporting any base image
is an eventual goal, however there are a few reasons why
this doesn't yet work.  The biggest reason is SELinux
labeling support; the underlying ostree stack currently
handles this and requires that the "base image"
have a pre-computed set of labels that can be used
for any derived layers.

# Building bootc compatible base images

As a corollary to base-image limitations, the build process
for generating base images currently requires running
through ostree tooling to generate an "ostree commit"
which has some special formatting in the base image.

However, the ostree usage is an implementation detail
and the requirement on this will be lifted in the future.

For example, the [rpm-ostree compose image](https://coreos.github.io/rpm-ostree/container/#creating-base-images)
tooling currently streamlines creating base images, operating just
on a declarative input and writing to a registry.

# Deriving from existing base images

However, it's important to emphasize that from one
of these specially-formatted base images, every
tool and technique for container building applies!
In other words it will Just Work to do
```
FROM <bootc base image>
RUN dnf -y install foo && dnf clean all 
```

## Using the `ostree container commit` command

As an opt-in optimization today, you can also add `ostree container commit`
as part of your `RUN` invocations.   This will perform early detection
of some incompatibilities but is not a strict requirement today and will not be
in the future.

# Building and using `bootc` compatible images in the real world

As of November 2023, there are two primary ways of generating a 
`bootc` compatible base image and both require the use of `rpm-ostree`.

The easiest and most straight-forward way is to use `rpm-ostree compose image`
in order to produce a `bootc` compatible OCI image from a [treefile](https://coreos.github.io/rpm-ostree/treefile/)
or manifest file. This is the most direct way to be able to produce an OCI
image that can be pushed to a registry or used by other container tools.

The alternative method is to produce and `ostree` commit
(i.e. using `rpm-ostree compose commit`) and then encapsulating the `ostree`
commit in an OCI image via `rpm-ostree compose container-encapsulate`. This
may be preferable if you already have a build process that produces `ostree`
commits as an output (e.g. using osbuild to produce `ostree` commit artifacts.)

The requirement for both methods is that your treefile/manifest **MUST** include
the `bootc` package in list of packages included in your compose.

## Building a custom `bootc` compatible image

With your base image created, you can now iterate on the customizations using
well known container build tools such as `podman build`, `buildah`, or 
`docker build`.

For example:

```Dockerfile
FROM quay.io/company_org/custom_os/bootc-base-image:latest
RUN dnf -y install strace && \
    dnf clean all && \
    ostree container commit
```

Since the base image and resulting custom image are OCI artifacts, any tool in the
container ecosystem that understands OCI images should be able to operate on the images.

## Installing a `bootc` compatible image

The interface for installing these compatible images resides in the `bootc` tool.
Users have the ability to use `bootc install` to install the contents of the
compatible container image directly to a block device. Alternatively, users can
use `bootc install-to-filesystem` to install the contents of the compatible
container image to an existing filesystem on a host.

These methods can be [driven interactively](https://github.com/containers/bootc/blob/main/docs/install.md#using-bootc-install-to-filesystem---replacealongside) 
by a user on an existing Linux system with `podman` and `skopeo` installed. It is feasible
that a user could write a cloud-config snippet to drive the `bootc` install methods on
a cloud VM that supports `cloud-init`.

**NOTE:** The current implementation of the `install` and `install-to-filesystem` methods
require that `bootc` be run **from** the compatible container image. This means
you are not able to install `bootc` to an existing system and install your compatible
container image. Failure to do so will result in the following error:

`ERROR Querying container: This command must be executed inside a podman container (missing run/.containerenv`

If users want to install a compatible container image as a day 1 operation, it is
possible to use [Anaconda](https://pykickstart.readthedocs.io/en/latest/kickstart-docs.html#ostreecontainer) 
to install the compatible container image to a system. In the future, it will be
possible to use [osbuild to generate disk images](https://github.com/osbuild/osbuild/pull/1418) 
from a `bootc` compatible container image.

## Updating your system with `bootc`

After your `bootc` compatible image has been installed and you've successfully
rebooted your system, you will now have the `bootc` tool available at the OS level.
You'll use `bootc upgrade` to instruct the system to pull the latest version of
your compatible container image and apply it to your system.

## Switching the `bootc` compatible container image

After rebooting into your compatible container image, it is possible to switch which
container image you are upgrading from. This is akin to the `rpm-ostree rebase` command.
You can make this switch by using the `bootc switch` command to point your system
at a new compatible container image.