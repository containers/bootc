# Filesystem

As noted in other chapters, the bootc project inherits
a lot of code from the [ostree project](https://github.com/ostreedev/ostree/).

However, bootc is intending to be a "fresh, new container-native interface".

First, it is strongly recommended that bootc consumers use the ostree
[composefs backend](https://ostreedev.github.io/ostree/composefs/); to do this,
ensure that you have a `/usr/lib/ostree/prepare-root.conf` that contains at least

```ini
[composefs]
enabled = true
```

This will ensure that the entire `/` is a read-only filesystem.

## `/usr`

The overall recommendation is to keep all operating system content in `/usr`.  See [UsrMove](https://fedoraproject.org/wiki/Features/UsrMove) for example.

## `/etc`

The `/etc` directory contains mutable persistent state by default; however,
it is suppported to enable the [`etc.transient` config option](https://ostreedev.github.io/ostree/man/ostree-prepare-root.html).

When in persistent mode, it inherits the OSTree semantics of [performing a 3-way merge](https://ostreedev.github.io/ostree/atomic-upgrades/#assembling-a-new-deployment-directory)
across upgrades.  In a nutshell:

- The *new default* `/etc` is used as a base
- The diff between current and previous `/etc` is applied to the new `/etc`
- Locally modified files in `/etc` different from the default `/usr/etc` (of the same deployment) will be retained

The implmentation of this defaults to being executed by `ostree-finalize-staged.service`
at shutdown time, before the new bootloader entry is created.

The rationale for this design is that in practice today, many components of a Linux system end up shipping
default configuration files in `/etc`.  And even if the default package doesn't, often the software
only looks for config files there by default.

Some other image-based update systems do not have distinct "versions" of `/etc` and
it may be populated only set up at a install time, and untouched thereafter.  But
that creates "hysteresis" where the state of the system's `/etc` is strongly
influenced by the initial image version.  This can lead to problems
where e.g. a change to `/etc/sudoers.conf` (to give on simple example)
would require external intervention to apply.

For more on configuration file best practices, see [Building](building/guidance.md).

## `/var`

Content in `/var` persists by default; it is however supported to make it or subdirectories
mount points (whether network or `tmpfs`).  There is exactly one `/var`.  If it is
not a distinct partition, then "physically" currently it is a bind mount into
`/ostree/deploy/$stateroot/var` and shared across "deployments" (bootloader entries).

As of OSTree v2024.3, by default [content in /var acts like a Docker VOLUME /var](https://github.com/ostreedev/ostree/pull/3166/commits/f81b9fa1666c62a024d5ca0bbe876321f72529c7).

This means that the content from the container image is copied at initial installation time, and *not updated thereafter*.

Note this is very different from the handling of `/etc`.   The rationale for this is
that `/etc` should generally only hold small text files, but `/var` should hold arbitrarily
large data (system logs, databases, etc.).  Creating multiple copies of it would have

to keep operating system upgrades from touching machine-local data by default.
If the system is rolled back to a previous bootloader entry, the `/var` content remains.  This also
makes it possible to "stage" new operating system updates in an alternative root without affecting `/var` content.

A common case is for applications to want some directory structure (e.g. `/var/lib/postgresql`) to be pre-created.
It's recommended to use [systemd tmpfiles.d](https://www.freedesktop.org/software/systemd/man/latest/tmpfiles.d.html)
for this.  An even better approach where applicable is [StateDirectory=](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html#RuntimeDirectory=)
in units.

## Other directories

It is not supported to ship content in `/run` or `/proc` or other [API Filesystems](https://www.freedesktop.org/wiki/Software/systemd/APIFileSystems/) in container images.

Besides those, for other toplevel directories such as `/usr` `/opt`, they will be lifecycled with the container image.

### `/opt`

In the default suggested model of using composefs (per above) the `/opt` directory will be read-only, alongside
other toplevels such as `/usr`.

Some software expects to be able to write to its own directory in `/opt/exampleapp`.  For these
cases, there are several options (containerizing the app, running it in a system unit that sets up custom mounts, etc.)

#### Enabling transient root

However, some use cases may find it easier to enable a fully transient writable rootfs by default.
To do this, set the

```toml
[root]
transient = true
```

option in `prepare-root.conf`.  In particular this will allow software to write (transiently) to `/opt`,
with symlinks to `/var` for content that should persist.