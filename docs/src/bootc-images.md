# "bootc compatible" images

At the current time, it does not work to just do:

```Dockerfile
FROM fedora
RUN dnf -y install kernel
```

or

```Dockerfile
FROM debian
RUN apt install linux
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

The two most common ways to do this are to either:

  1. compose a compatible OCI image directly via [`rpm-ostree compose image`](https://coreos.github.io/rpm-ostree/container/#creating-base-images)
  1. encapsulate an ostree commit using `rpm-ostree compose container-encapsulate`

The first method is most direct, as it streamlines the process of
creating a base image and writing to a registry. The second method
may be preferable if you already have a build process that produces `ostree`
commits as an output (e.g. using [osbuild](https://www.osbuild.org/guides/image-builder-on-premises/building-ostree-images.html)
to produce `ostree` commit artifacts.)

The requirement for both methods is that your initial treefile/manifest
**MUST** include the `bootc` package in list of packages included in your compose.

However, the ostree usage is an implementation detail
and the requirement on this will be lifted in the future.

## Standard metadata for bootc compatible images

It is strongly recommended to do:

```dockerfile
LABEL containers.bootc 1
```

This will signal that this image is intended to be usable with `bootc`.

# Deriving from existing base images

It's important to emphasize that from one
of these specially-formatted base images, every
tool and technique for container building applies!
In other words it will Just Work to do

```Dockerfile
FROM <bootc base image>
RUN dnf -y install foo && dnf clean all
```

You can then use `podman build`, `buildah`, `docker build`, or any other container
build tool to produce your customized image. The only requirement is that the
container build tool supports producing OCI container images.

## The `ostree container commit` command

You may find some references to this; it is no longer very useful
and is not recommended.

# The bootloader setup

At the current time bootc relies on the [bootupd](https://github.com/coreos/bootupd/)
project which handles bootloader installs and upgrades.  The invocation of
`bootc install` will always run `bootupd` to perform installations.
Additionally, `bootc upgrade` will currently not upgrade the bootloader;
you must invoke `bootupctl update`.
