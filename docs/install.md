# Understanding `bootc install`

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

The `bootc install` command bridges the two worlds of a standard runnable OCI image
and a bootable system by running tooling
logic embedded in the container image to create the filesystem and
bootloader setup dynamically, using tools already embedded in the container
image.  This requires running the container via `--privileged`; it uses
the running Linux kernel to write the file content from the running container image;
not the kernel inside the container.

However nothing *else* (external) is required to perform a basic installation
to disk.  This is motivated by experience gained from the Fedora CoreOS
project where today the expectation is that one boots from a pre-existing disk
image (AMI, qcow2, etc) or use [coreos-installer](https://github.com/coreos/coreos-installer)
for many bare metal setups.  But the problem is that coreos-installer
is oriented to installing raw disk images.  This means that if
one creates a custom derived container, then it's required for
one to also generate a raw disk image to install.  This is a large
ergonomic hit.

With `bootc install`, no extra steps are required.  Every container
image comes with a basic installer.

## Executing `bootc install`

The installation command must be run from the container image
that will be installed, using `--privileged` and a few
other options.

Here's an example:

```
$ podman run --privileged --pid=host --net=none --security-opt label=type:unconfined_t ghcr.io/cgwalters/c9s-oscore bootc install --target-no-signature-verification /path/to/disk
```

Note that while `--privileged` is used, this command will not
perform any destructive action on the host system.

The `--pid=host --security-opt label=type:unconfined_t` today
make it more convenient for bootc to perform some privileged
operations; in the future these requirement may be dropped.

The `--net=none` argument is just to emphasize the fact that
an installation by default is not fetching anything else external
from the network - the content to be installed
*is the running container image content*.

### Note: Today `bootc install` has a host requirement on `skopeo`

The one exception to host requirements today is that the host must
have `skopeo` installed.  This is a bug; more information in [this issue](https://github.com/containers/bootc/issues/81).


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

But a new super-power with `bootc` is that you can also easily instead
create a derived container that injects your desired configuration,
alongside any additional executable code (packages, etc).

The expectation is that most operating systems will be designed such
that user state i.e. `/root` and `/home` will be on a separate persistent data store.
For example, in the default ostree model, `/root` is `/var/roothome`
and `/home` is `/var/home`.  Content in `/var` cannot be shipped
in the image - it is per machine state.

#### Injecting SSH keys in a container image

In this example, we will configure OpenSSH to read the
set of authorized keys for the root user from content
that lives in `/usr` (i.e. is owned by the container image).
We will also create a `/usr/etc-system` directory which is intentionally distinct
from the default ostree `/etc` which may be locally writable.

The `AuthorizedKeysFile` invocation below then configures sshd to look
for keys in this location.

```
FROM ghcr.io/cgwalters/c9s-oscore
RUN mkdir -p /usr/etc-system/ && \
    echo 'AuthorizedKeysFile /usr/etc-system/%u.keys' >> /etc/ssh/sshd_config.d/30-auth-system.conf && \
    echo 'ssh-ed25519 AAAAC3Nza... root@example.com' > /usr/etc-system/root.keys && chmod 0600 /usr/etc-system/keys && \
    ostree container commit
```

A key point here is that now the set of authorized keys is "owned"
by the container image - it will be read-only at runtime because
the files are underneath `/usr`.  To rotate or change the set of keys,
one would build a new container image.  Client systems using `bootc upgrade`
will transactionally update to this new system state.


## More advanced installation

The basic `bootc install` logic is really a pretty small (but opinionated) wrapper
for a set of lower level tools that can also be invoked independently.

The `bootc install` command is effectively:

- `mkfs.$fs /dev/disk`
- `mount /dev/disk /mnt`
- `bootc install-to-filesystem --karg=root=UUID=<uuid of /mnt> --imgref $self /mnt`

There may be a bit more involved here; for example configuring
`--block-setup tpm2-luks` will configure the root filesystem
with LUKS bound to the TPM2 chip, currently via [systemd-cryptenroll](https://www.freedesktop.org/software/systemd/man/systemd-cryptenroll.html#).

Some OS/distributions may not want to enable it at all; it
can be configured off at build time via Cargo features.

### Using `bootc install-to-filesystem`

As noted above, there is also `bootc install-to-filesystem`, which allows
an arbitrary process to create the root filesystem.

The usual expected way for an external storage system to work
is to provide `root=<UUID>` type kernel arguments.  At the current
time a separate `/boot` filesystem is also required (mainly to enable LUKS)
so you will also need to provide e.g. `--boot-mount-spec UUID=...`.

The `bootc install-to-filesystem` command allows an operating
system or distribution to ship a separate installer that creates more complex block
storage or filesystem setups, but reuses the "top half" of the logic.
For example, a goal is to change [Anaconda](https://github.com/rhinstaller/anaconda/)
to use this.


### Using `bootc install-to-filesystem --replace=alongside`

This is a variant of `install-to-filesystem`, which maximizes convenience for using
an existing Linux system, converting it into the target container image.  Note that
the `/boot` (and `/boot/efi`) partitions *will be reinitialized* - so this is a
somewhat destructive operation for the existing Linux installation.

Also, because the filesystem is reused, it's required that the target system kernel
support the root storage setup already initialized.

The core command should look like this:

```
$ podman run --privileged -v /:/target --pid=host --net=none --security-opt label=type:install_t \
  <image> \
  bootc install-to-filesystem --replace=alongside /target
```

At the current time, leftover data in `/` is *not* automatically cleaned up.  This can
be useful, because it allows the new image to automatically import data from the previous
host system!  For example, things like SSH keys or container images can be copied
and then deleted from the original.
