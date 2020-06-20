# bootupd: Distribution-independent updates for bootloaders

Today many Linux systems handle updates for bootloader data
in an inconsistent and ad-hoc way.  For example, on
Fedora and Debian, a package manager update will update UEFI
binaries in `/boot/efi`, but not the BIOS MBR data.

Transactional update systems like [OSTree](https://github.com/ostreedev/ostree/) and others
normally cover kernel/userspace but not bootloaders, because
performing bootloader updates in an "A/B" fashion requires
completely separate nontrivial logic. Today OSTree e.g.
makes the choice that it does not update `/boot/efi`.

The goal of this project is to be a cross-distribution,
OS update system agnostic tool to manage updates for things like:

- `/boot/efi`
- x86 BIOS MBR
- Other architecture bootloaders

This project originated in [this Fedora CoreOS github issue](https://github.com/coreos/fedora-coreos-tracker/issues/510).

## Status

Currently a work in progress and is not ready to ship for production
updates, but early feedback on the design is appreciated!
