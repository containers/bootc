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

## Kernel

The Linux kernel (and optionally initramfs) is embedded in the container image; the canonical location
is `/usr/lib/modules/$kver/vmlinuz`, and the initramfs should be in `initramfs.img`
in that directory. You should *not* include any content in `/boot` in your container image.
Bootc will take care of copying the kernel/initramfs as needed from the container image to
`/boot`.

Future work for supporting UKIs will follow the recommendations of the uapi-group in [Locations for Distribution-built UKIs Installed by Package Managers](https://uapi-group.org/specifications/specs/unified_kernel_image/#locations-for-distribution-built-ukis-installed-by-package-managers).

## The `ostree container commit` command

You may find some references to this; it is no longer very useful
and is not recommended.

# The bootloader setup

At the current time bootc relies on the [bootupd](https://github.com/coreos/bootupd/)
project which handles bootloader installs and upgrades.  The invocation of
`bootc install` will always run `bootupd` to perform installations.
Additionally, `bootc upgrade` will currently not upgrade the bootloader;
you must invoke `bootupctl update`.

# SELinux

Container runtimes such as `podman` and `docker` commonly
apply a "coarse" SELinux policy to running containers.
See [container-selinux](https://github.com/containers/container-selinux/blob/main/container_selinux.8).
It is very important to understand that non-bootc base
images do not (usually) have any embedded `security.selinux` metadata
at all; all labels on the toplevel container image
are *dynamically* generated per container invocation,
and there are no individually distinct e.g. `etc_t` and
`usr_t` types.

In contrast, with the current OSTree backend for bootc,
when the base image is built, label metadata is included
in special metadata files in `/sysroot/ostree` that correspond
to components of the base image.

When a bootc container is deployed, the system
will use these default SELinux labels.
Further non-OSTree layers will be dynamically labeled
using the base policy.

Hence, at the current time it will *not* work to override
the labels for files in derived layers by using e.g.

```
RUN semanage fcontext -a -t httpd_sys_content_t "/web(/.*)?"
```

(This command will write to `/etc/selinux/$policy/policy/`.)

It will *never* work to do e.g.:

```
RUN chcon -t foo_t /usr/bin/foo
```

Because the container runtime state will deny the attempt to
"physically" set the `security.selinux` extended attribute.
In contrast per above, future support for custom labeling
will by default be done by customizing the policy file_contexts.

### Toplevel directories

In particular, a common problem is that inside a container image,
it's easy to create arbitrary toplevel directories such as
e.g. `/app` or `/aimodel` etc.  But in some SELinux policies
such as Fedora derivatives, these will be labeled as `default_t`
which few domains can access.

References:

- <https://github.com/ostreedev/ostree-rs-ext/issues/510>
