![bootc logo](https://raw.githubusercontent.com/containers/common/main/logos/bootc-logo-full-vert.png)
# bootc

Transactional, in-place operating system updates using OCI/Docker container images.

## Motivation

The original Docker container model of using "layers" to model
applications has been extremely successful.  This project
aims to apply the same technique for bootable host systems - using
standard OCI/Docker containers as a transport and delivery format
for base operating system updates.

The container image includes a Linux kernel (in e.g. `/usr/lib/modules`),
which is used to boot.  At runtime on a target system, the base userspace is
*not* itself running in a "container" by default. For example, assuming
systemd is in use, systemd acts as pid1 as usual - there's no "outer" process.
More about this in the docs; see below.

## Status

The CLI and API are considered stable. We will ensure that every existing system
can be upgraded in place seamlessly across any future changes.

## Documentation

See the [project documentation](https://bootc-dev.github.io/bootc/).

## Adopters (base and end-user images)

The bootc CLI is just a client system; it is not tied to any particular
operating system or Linux distribution. You very likely want to actually
start by looking at [ADOPTERS.md](ADOPTERS.md).

## Community discussion

The [Github discussion forum](https://github.com/containers/bootc/discussions) is enabled.

This project is also tightly related to the previously mentioned Fedora/CentOS bootc project,
and many developers monitor the relevant discussion forums there. In particular there's a
Matrix channel and a weekly video call meeting for example: <https://docs.fedoraproject.org/en-US/bootc/community/>.

## Developing bootc

Are you interested in working on bootc?  Great!  See our [CONTRIBUTING.md](CONTRIBUTING.md) guide.
There is also a list of [MAINTAINERS.md](MAINTAINERS.md).

