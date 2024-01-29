---
nav_order: 2
---

# Installing "bootc compatible" images

A key goal of the bootc project is to think of bootable operating systems
as container images.  Docker/OCI container images are just tarballs
wrapped with some JSON.  But in order to boot a system (whether on bare metal
or virtualized), one needs a few key components:

- bootloader
- kernel (and optionally initramfs)
- root filesystem (xfs/ext4/btrfs etc.)

The Linux kernel (and optionally initramfs) is embedded in the container image; the canonical location
is `/usr/lib/modules/$kver/vmlinuz`, and the initramfs should be in `initramfs.img`
in that directory.

The `bootc install` command bridges the two worlds of a standard, runnable OCI image
and a bootable system by running tooling logic embedded
in the container image to create the filesystem and bootloader setup dynamically.
This requires running the container via `--privileged`; it uses the running Linux kernel
on the host to write the file content from the running container image; not the kernel
inside the container.

There are two sub-commands: `bootc install to-disk` and `boot install to-filesystem`.

However, nothing *else* (external) is required to perform a basic installation
to disk.  (The one exception to host requirements today is that the host must
have `skopeo` installed.  This is a bug; more information in
[this issue](https://github.com/containers/bootc/issues/81).)

This is motivated by experience gained from the Fedora CoreOS
project where today the expectation is that one boots from a pre-existing disk
image (AMI, qcow2, etc) or uses [coreos-installer](https://github.com/coreos/coreos-installer)
for many bare metal setups.  But the problem is that coreos-installer
is oriented to installing raw disk images.  This means that if
one creates a custom derived container, then it's required for
one to also generate a raw disk image to install.  This is a large
ergonomic hit.

With the `bootc` install methods, no extra steps are required.  Every container
image comes with a basic installer.

## Executing `bootc install`

The two installation commands allow you to install the container image
either directly to a block device (`bootc install to-disk`) or to an existing
filesystem (`bootc install to-filesystem`).

The installation commands **MUST** be run **from** the container image
that will be installed, using `--privileged` and a few
other options. This means you are (currently) not able to install `bootc`
to an existing system and install your container image. Failure to run
`bootc` from a container image will result in an error.

Here's an example of using `bootc install` (root/elevated permission required):

```bash
podman run --rm --privileged --pid=host --security-opt label=type:unconfined_t <image> bootc install to-disk /path/to/disk
```

Note that while `--privileged` is used, this command will not perform any
destructive action on the host system.  Among other things, `--privileged`
makes sure that all host devices are mounted into container. `/path/to/disk` is
the host's block device where `<image>` will be installed on.

The `--pid=host --security-opt label=type:unconfined_t` today
make it more convenient for bootc to perform some privileged
operations; in the future these requirement may be dropped.

Jump to the section for [`install to-filesystem`](#more-advanced-installation) later
in this document for additional information about that method.

### "day 2" updates, security and fetch configuration

By default the `bootc install` path will find the pull specification used
for the `podman run` invocation and use it to set up "day 2" OS updates that `bootc update`
will use.

For example, if you invoke `podman run --privileged ... quay.io/examplecorp/exampleos:latest bootc install ...`
then the installed operating system will fetch updates from `quay.io/examplecorp/exampleos:latest`.
This can be overridden via `--target_imgref`; this is handy in cases like performing
installation in a manufacturing environment from a mirrored registry.

By default, the installation process will verify that the container (representing the target OS)
can fetch its own updates.

Additionally note that to perform an install with a target image reference set to an
authenticated registry, you must provide a pull secret.  One path is to embed the pull secret into
the image in `/etc/ostree/auth.json`.
Alternatively, the secret can be added after an installation process completes and managed separately;
in that case you will need to specify `--skip-fetch-check`.

### Operating system install configuration required

The container image **MUST** define its default install configuration.  A key choice
that bootc by default leaves up to the operating system image is the root filesystem
type.

To enable `bootc install` as part of your OS/distribution base image,
create a file named `/usr/lib/bootc/install/00-<osname>.toml` with the contents of the form:

```toml
[install.filesystem.root]
type = "xfs"
```

The `install.filesystem.root` value **MUST** be set.

Configuration files found in this directory will be merged, with higher alphanumeric values
taking precedence.  If for example you are building a derived container image from the above OS,
you could create a `50-myos.toml`  that sets `type = "btrfs"` which will override the
prior setting.

Other available options, also under the `[install]` section:

`kargs`: This allows setting kernel arguments which apply only at the time of `bootc install`.
This option is particularly useful when creating derived/layered images; for example, a cloud
image may want to have its default `console=` set, in contrast with a default base image.
The values in this field are space separated.

`root-fs-type`: This value is the same as `install.filesystem.root.type`.

## Installing an "unconfigured" image

The bootc project aims to support generic/general-purpose operating
systems and distributions that will ship unconfigured images.  An
unconfigured image does not have a default password or SSH key, etc.

There are two fundamental ways to handle this:

### Using cloud-init type flows

Some operating systems may come with `cloud-init` or similar tools
that know how to e.g. inject SSH keys or external configuration.

Other tools in this space are:

- [systemd-firstboot](https://www.freedesktop.org/software/systemd/man/systemd-firstboot.html)
- [gnome-initial-setup](https://gitlab.gnome.org/GNOME/gnome-initial-setup)

The general idea here is that things like users, passwords and ssh keys
are dynamically created on first boot (and in general managed per-system);
the configuration comes from a place *external* to the image.

### Injecting configuration into a custom image

But a new super-power with `bootc` is that you can also easily
create a derived container that injects your desired configuration,
alongside any additional executable code (binaries, packages, scripts, etc).

The expectation is that most operating systems will be designed such
that user state i.e. `/root` and `/home` will be on a separate, persistent data store.
For example, in the default ostree model, `/root` is `/var/roothome`
and `/home` is `/var/home`.  Content in `/var` cannot be shipped
in the image - it is per machine state.

#### Injecting SSH keys in a container image

In the following example, we will configure OpenSSH to read the
set of authorized keys for the root user from content
that lives in `/usr` (i.e. is owned by the container image).
We will also create a `/usr/etc-system` directory which is intentionally distinct
from the default ostree `/etc` which may be locally writable.

The `AuthorizedKeysFile` invocation below then configures sshd to look
for keys in this location.

```Dockerfile
FROM <image>
RUN mkdir -p /usr/etc-system/ && \
    echo 'AuthorizedKeysFile /usr/etc-system/%u.keys' >> /etc/ssh/sshd_config.d/30-auth-system.conf && \
    echo 'ssh-ed25519 AAAAC3Nza... root@example.com' > /usr/etc-system/root.keys && chmod 0600 /usr/etc-system/root.keys && \
    ostree container commit
```

A key point here is that now the set of authorized keys is "owned"
by the container image - it will be read-only at runtime because
the files are underneath `/usr`.  To rotate or change the set of keys,
one would build a new container image.  Client systems using `bootc upgrade`
will transactionally update to this new system state.

## More advanced installation with `to-filesystem`

The basic `bootc install to-disk` logic is really a pretty small (but opinionated) wrapper
for a set of lower level tools that can also be invoked independently.

The `bootc install to-disk` command is effectively:

- `mkfs.$fs /dev/disk`
- `mount /dev/disk /mnt`
- `bootc install to-filesystem --karg=root=UUID=<uuid of /mnt> --imgref $self /mnt`

There may be a bit more involved here; for example configuring
`--block-setup tpm2-luks` will configure the root filesystem
with LUKS bound to the TPM2 chip, currently via [systemd-cryptenroll](https://www.freedesktop.org/software/systemd/man/systemd-cryptenroll.html#).

Some OS/distributions may not want to enable it at all; it
can be configured off at build time via Cargo features.

### Using `bootc install to-filesystem`

The usual expected way for an external storage system to work
is to provide `root=<UUID>` type kernel arguments.  At the current
time a separate `/boot` filesystem is also required (mainly to enable LUKS)
so you will also need to provide e.g. `--boot-mount-spec UUID=...`.

The `bootc install to-filesystem` command allows an operating
system or distribution to ship a separate installer that creates more complex block
storage or filesystem setups, but reuses the "top half" of the logic.
For example, a goal is to change [Anaconda](https://github.com/rhinstaller/anaconda/)
to use this.

### Using `bootc install to-disk --via-loopback`

Because every `bootc` system comes with an opinionated default installation
process, you can create a raw disk image (that can e.g. be booted via virtualization)
via e.g.:

```bash
truncate -s 10G exampleos.raw
podman run --rm --privileged --pid=host --security-opt label=type:unconfined_t -v .:/output <yourimage> bootc install to-disk --generic-image --via-loopback /output/myimage.raw
```

Notice that we use `--generic-image` for this use case.

### Using `bootc install to-filesystem --replace=alongside`

This is a variant of `install to-filesystem`, which maximizes convenience for using
an existing Linux system, converting it into the target container image.  Note that
the `/boot` (and `/boot/efi`) partitions *will be reinitialized* - so this is a
somewhat destructive operation for the existing Linux installation.

Also, because the filesystem is reused, it's required that the target system kernel
support the root storage setup already initialized.

The core command should look like this (root/elevated permission required):

```bash
podman run --rm --privileged -v /:/target \
             --pid=host --security-opt label=type:unconfined_t \
             <image> \
             bootc install to-filesystem --replace=alongside /target
```

At the current time, leftover data in `/` is **NOT** automatically cleaned up.  This can
be useful, because it allows the new image to automatically import data from the previous
host system!  For example, things like SSH keys or container images can be copied
and then deleted from the original.

### Using `bootc install to-filesystem --source-imgref <imgref>`

By default, `bootc install` has to be run inside a podman container. With this assumption,
it can escape the container, find the source container image (including its layers) in
the podman's container storage and use it to create the image.

When `--source-imgref <imgref>` is given, `bootc` no longer assumes that it runs inside podman.
Instead, the given container image reference (see [containers-transports(5)](https://github.com/containers/image/blob/main/docs/containers-transports.5.md)
for accepted formats) is used to fetch the image. Note that `bootc install` still has to be
run inside a chroot created from the container image. However, this allows users to use
a different sandboxing tool (e.g. [bubblewrap](https://github.com/containers/bubblewrap)).

This argument is mainly useful for 3rd-party tooling for building disk images from bootable
containers (e.g. based on [osbuild](https://github.com/osbuild/osbuild)).   
