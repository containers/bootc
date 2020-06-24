# bootupd: Distribution-independent updates for bootloaders

Today many Linux systems handle updates for bootloader data
in an inconsistent and ad-hoc way.  For example, on
Fedora and Debian, a package manager update will update UEFI
binaries in `/boot/efi`, but not the BIOS MBR data.

Many transactional update systems like [OSTree](https://github.com/ostreedev/ostree/)
and dual-partition systems like the Container Linux update system
are more consistent: they normally cover kernel/userspace but not anything
related to bootloaders.

The reason for this is straightforward: performing bootloader
updates in an "A/B" fashion requires completely separate nontrivial
logic from managing the kernel and root filesystem.  Today OSTree e.g.
makes the choice that it does not update `/boot/efi` (and also doesn't
update the BIOS MBR).

The goal of this project is to be a cross-distribution,
OS update system agnostic tool to manage updates for things like:

- `/boot/efi`
- x86 BIOS MBR
- Other architecture bootloaders

This project originated in [this Fedora CoreOS github issue](https://github.com/coreos/fedora-coreos-tracker/issues/510).

## Status

Currently a work in progress and is not ready to ship for production
updates, but early feedback on the design is appreciated!

## Relationship to other projects

### fwupd

bootupd could be compared to [fwupd](https://github.com/fwupd/fwupd/) which is
project that exists today to update hardware device firmware - things not managed
by e.g. `apt/zypper/yum/rpm-ostree update` today.

fwupd comes as a UEFI binary today, so bootupd would actually take care of updating fwupd itself.

The end result is that a system administrator would have 3 projects to monitor
(one for hardware devices and 2 for the bootloader and OS state) but each
would be clearly focused on its domain and handle it well.  This stands
in contrast with e.g. having an RPM `%post` script try to regenerate the BIOS MBR.

### systemd bootctl

[systemd bootctl](https://man7.org/linux/man-pages/man1/bootctl.1.html) can update itself;
this project would probably just proxy that if we detect systemd-boot is in use.
