# Generic guidance for building images

The bootc project intends to be operating system and distribution independent as possible,
similar to its related projects [podman](http://podman.io/) and [systemd](https://systemd.io/),
etc.

The recommendations for creating bootc-compatible images will in general need to
be owned by the OS/distribution - in particular the ones who create the default
bootc base image(s). However, some guidance is very generic to most Linux
systems (and bootc only supports Linux).

Let's however restate a base goal of this project:

> The original Docker container model of using "layers" to model
> applications has been extremely successful.  This project
> aims to apply the same technique for bootable host systems - using
> standard OCI/Docker containers as a transport and delivery format
> for base operating system updates.

Every tool and technique for creating application base images
should apply to the host Linux OS as much as possible.

## Installing software

For package management tools like `apt`, `dnf`, `zypper` etc.
(generically, `$pkgsystem`) it is very much expected that
the pattern of

`RUN $pkgsystem install somepackage && $pkgsystem clean all`

type flow Just Works here - the same way as it does
"application" container images.  This pattern is really how
Docker got started.

There's not much special to this that doesn't also apply
to application containers; but see below.

## systemd units

The model that is most popular with the Docker/OCI world
is "microservice" style containers with the application as
pid 1, isolating the applications from each other and
from the host system - as opposed to "system containers"
which run an init system like systemd, typically also
SSH and often multiple logical "application" components
as part of the same container.

The bootc project generally expects systemd as pid 1,
and if you embed software in your derived image, the
default would then be that that software is initially
launched via a systemd unit.

```
RUN dnf -y install postgresql
```

Would typically also carry a systemd unit, and that
service will be launched the same way as it would
on a package-based system.

## Users and groups

Note that the above `postgresql` today will allocate a user;
this leads to the topic of [users, groups and SSH keys](users-and-groups.md).

## Configuration

A key aspect of choosing a bootc-based operating system model
is that *code* and *configuration* can be strictly "lifecycle bound"
together in exactly the same way.

(Today, that's by including the configuration into the base
 container image; however a future enhancement for bootc
 will also support dynamically-injected ConfigMaps, similar
 to kubelet)

You can add configuration files to the same places they're
expected by typical package systems on Debian/Fedora/Arch
etc. and others - in `/usr` (preferred where possible)
or `/etc`.  systemd has long advocated and supported
a model where `/usr` (e.g. `/usr/lib/systemd/system`)
contains content owned by the operating system image.

`/etc` is machine-local state.  However, per [filesystem.md](../filesystem.md)
it's important to note that the underlying OSTree
system performs a 3-way merge of `/etc`, so changes you
make in the container image to e.g. `/etc/postgresql.conf`
will be applied on update, assuming it is not modified
locally.

