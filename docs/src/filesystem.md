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

The `/etc` directory contains persistent state by default; however,
it is suppported to enable the [`etc.transient` config option](https://ostreedev.github.io/ostree/man/ostree-prepare-root.html).

When in persistent mode, it inherits the OSTree semantics of [performing a 3-way merge](https://ostreedev.github.io/ostree/atomic-upgrades/#assembling-a-new-deployment-directory)
across upgrades.

## `/var`

Content in `/var` persists by default; it is however supported to make it or subdirectories
mount points (whether network or `tmpfs`)

As of OSTree v2024.3, by default [content in /var acts like a Docker VOLUME /var](https://github.com/ostreedev/ostree/pull/3166/commits/f81b9fa1666c62a024d5ca0bbe876321f72529c7).

This means that the content from the container image is copied at *initial installation time*, and not updated thereafter.

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

```
[root]
transient = true
```

option in `prepare-root.conf`.  In particular this will allow software to write (transiently) to `/opt`,
with symlinks to `/var` for content that should persist.