
# Users and groups

This is one of the more complex topics. Generally speaking, bootc has nothing to
do directly with configuring users or groups; it is a generic OS
update/configuration mechanism. (There is currently just one small exception in
that `bootc install` has a special case `--root-ssh-authorized-keys` argument,
but it's very much optional).

## Generic base images

Commonly OS/distribution base images will be generic, i.e.
without any configuration.  It is *very strongly recommended*
to avoid hardcoded passwords and ssh keys with publicly-available
private keys (as Vagrant does) in generic images.

### Injecting SSH keys via systemd credentials

The systemd project has documentation for [credentials](https://systemd.io/CREDENTIALS/)
which can be used in some environments to inject a root
password or SSH authorized_keys.  For many cases, this
is a best practice.

At the time of this writing this relies on SMBIOS which
is mainly configurable in local virtualization environments.
(qemu).

### Injecting users and SSH keys via cloud-init, etc.

Many IaaS and virtualization systems are oriented towards a "metadata server"
(see e.g. [AWS instance metadata](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/ec2-instance-metadata.html))
that are commonly processed by software such as [cloud-init](https://cloud-init.io/)
or [Ignition](https://github.com/coreos/ignition) or equivalent.

The base image you're using may include such software, or you
can install it in your own derived images.

In this model, SSH configuration is managed outside of the bootable
image.  See e.g. [GCP oslogin](https://cloud.google.com/compute/docs/oslogin/)
for an example of this where operating system identities are linked
to the underlying Google accounts.

### Adding users and credentials via custom logic (container or unit)

Of course, systems like `cloud-init` are not privileged; you
can inject any logic you want to manage credentials via
e.g. a systemd unit (which may launch a container image)
that manages things however you prefer.  Commonly,
this would be a custom network-hosted source.  For example,
[FreeIPA](https://www.freeipa.org/page/Main_Page).

Another example in a Kubernetes-oriented infrastructure would
be a container image that fetches desired authentication
credentials from a [CRD](https://kubernetes.io/docs/tasks/extend-kubernetes/custom-resources/custom-resource-definitions/)
hosted in the API server.  (To do things like this
it's suggested to reuse the kubelet credentials)

### Adding users and credentials statically in the container build

Relative to package-oriented systems, a new ability is to inject
users and credentials as part of a derived build:

```dockerfile
RUN useradd someuser
```

However, it is important to understand some issues with the default
`shadow-utils` implementation of `useradd`:

First, typically user/group IDs are allocated dynamically, and this can result in "drift" (see below).

#### User and group home directories and `/var`

For systems configured with persistent `/home` → `/var/home`, any changes to `/var` made
in the container image after initial installation *will not be applied on subsequent updates*.  If for example you inject `/var/home/someuser/.ssh/authorized_keys`
into a container build, existing systems will *not* get the updated authorized keys file.

#### Using DynamicUser=yes for systemd units

For "system" users it's strongly recommended to use systemd [DynamicUser=yes](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html#DynamicUser=) where
possible.

This is significantly better than the pattern of allocating users/groups
at "package install time" (e.g. [Fedora package user/group guidelines](https://docs.fedoraproject.org/en-US/packaging-guidelines/UsersAndGroups/)) because
it avoids potential UID/GID drift (see below).

#### Using systemd-sysusers

See [systemd-sysusers](https://www.freedesktop.org/software/systemd/man/latest/systemd-sysusers.html).  For example in your derived build:

```
COPY mycustom-user.conf /usr/lib/sysusers.d
```

A key aspect of how this works is that `sysusers` will make changes
to the traditional `/etc/passwd` file as necessary on boot.  If
`/etc` is persistent, this can avoid uid/gid drift (but
in the general case it does mean that uid/gid allocation can
depend on how a specific machine was upgraded over time).

#### Using systemd JSON user records

See [JSON user records](https://systemd.io/USER_RECORD/).  Unlike `sysusers`,
the canonical state for these live in `/usr` - if a subsequent
image drops a user record, then it will also vanish
from the system - unlike `sysusers.d`.

#### nss-altfiles

The [nss-altfiles](https://github.com/aperezdc/nss-altfiles) project
(long) predates systemd JSON user records.  It aims to help split
"system" users into `/usr/lib/passwd` and `/usr/lib/group`.  It's
very important to understand that this aligns with the way
the OSTree project handles the "3 way merge" for `/etc` as it
relates to `/etc/passwd`.  Currently, if the `/etc/passwd` file is
modified in any way on the local system, then subsequent changes
to `/etc/passwd` in the container image *will not be applied*.

Some base images may have `nss-altfiles` enabled by default;
this is currently the case for base images built by
[rpm-ostree](https://github.com/coreos/rpm-ostree).

Commonly, base images will have some "system" users pre-allocated
and managed via this file again to avoid uid/gid drift.

In a derived container build, you can also append users
to `/usr/lib/passwd` for example.  (At the time of this
writing there is no command line to do so though).

Typically it is more preferable to use `sysusers.d`
or `DynamicUser=yes`.

### Machine-local state for users

At this point, it is important to understand the [filesystem](filesystem.md)
layout - the default is up to the base image.

The default Linux concept of a user has data stored in both `/etc` (`/etc/passwd`, `/etc/shadow` and groups)
and `/home`.  The choice for how these work is up to the base image, but
a common default for generic base images is to have both be machine-local persistent state.
In this model `/home` would be a symlink to `/var/home/someuser`.

#### Injecting users and SSH keys via at system provisioning time

For base images where `/etc` and `/var` are configured to persist by default, it
will then be generally supported to inject users via "installers" such
as [Anaconda](https://github.com/rhinstaller/anaconda/) (interactively or
via kickstart) or any others.

Typically generic installers such as this are designed for "one time bootstrap"
and again then the configuration becomes mutable machine-local state
that can be changed "day 2" via some other mechanism.

The simple case is a user with a password - typically the installer helps
set the initial password, but to change it there is a different in-system
tool (such as `passwd` or a GUI as part of [Cockpit](https://cockpit-project.org/), GNOME/KDE/etc).

It is intended that these flows work equivalently in a bootc-compatible
system, to support users directly installing "generic" base images, without
requiring changes to the tools above.

#### Transient home directories

Many operating system deployments will want to minimize persistent,
mutable and executable state - and user home directories are that

But it is also valid to default to having e.g. `/home` be a `tmpfs`
to ensure user data is cleaned up across reboots (and this pairs particularly
well with a transient `/etc` as well):

In order to set up the user's home directory to e.g. inject SSH `authorized_keys`
or other files, a good approach is to use systemd `tmpfiles.d` snippets:

```
f~ /home/someuser/.ssh/authorized_keys 600 someuser someuser - <base64 encoded data>
```
which can be embedded in the image as `/usr/lib/tmpfiles.d/someuser-keys.conf`.

Or a service embedded in the image can fetch keys from the network and write
them; this is the pattern used by cloud-init and [afterburn](https://github.com/coreos/afterburn).

### UID/GID drift

Ultimately the `/etc/passwd` and similar files are a mapping
between names and numeric identifiers.  A problem then becomes
when this mapping is dynamic and mixed with "stateless"
container image builds.

For example today the CentOS Stream 9 `postgresql` package
allocates a [static uid of `26`](https://gitlab.com/redhat/centos-stream/rpms/postgresql/-/blob/a03cf81d4b9a77d9150a78949269ae52a0027b54/postgresql.spec#L847).

This means that
```
RUN dnf -y install postgresql
```

will always result in a change to `/etc/passwd` that allocates uid 26
and data in `/var/lib/postgres` will always be owned by that UID.

However in contrast, the cockpit project allocates
[a floating cockpit-ws user](https://gitlab.com/redhat/centos-stream/rpms/cockpit/-/blob/1909236ad28c7d93238b8b3b806ecf9c4feb7e46/cockpit.spec#L506).

This means that each container image build (without additional work)
may (due to RPM installation ordering or other reasons) result
in the uid changing.

This can be a problem if that user maintains persistent state.
Such cases are best handled by being converted to use `sysusers.d`
(see [Fedora change](https://fedoraproject.org/wiki/Changes/Adopting_sysusers.d_format)) - or again even better, using `DynamicUser=yes` (see above).

