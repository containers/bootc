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

The scope is otherwise limited; for example, bootupd will not
manage anything related to the kernel such as kernel arguments;
that's for tools like `grubby` and `ostree`.

## Status

Currently a work in progress and is not ready to ship for production
updates, but early feedback on the design is appreciated!

## Relationship to other projects

### dbxtool

[dbxtool](https://github.com/rhboot/dbxtool) manages updates
to the Secure Boot database - `bootupd` will likely need to
perform any updates to the `shimx64.efi` binary
*before* `dbxtool.service` starts.  But otherwise they are independent.

### fwupd

bootupd could be compared to [fwupd](https://github.com/fwupd/fwupd/) which is
a project that exists today to update hardware device firmware - things not managed
by e.g. `apt/zypper/yum/rpm-ostree update` today.

fwupd comes as a UEFI binary today, so bootupd *could* take care of updating `fwupd`
but today fwupd handles that itself.  So it's likely that bootupd would only take
care of GRUB and shim.  See discussion in [this issue](https://github.com/coreos/bootupd/issues/1).

### systemd bootctl

[systemd bootctl](https://man7.org/linux/man-pages/man1/bootctl.1.html) can update itself;
this project would probably just proxy that if we detect systemd-boot is in use.

## Other goals

One idea is that bootupd could help support [redundant bootable disks](https://github.com/coreos/fedora-coreos-tracker/issues/581).
For various reasons it doesn't really work to try to use RAID1 for an entire disk; the ESP must be handled
specially.  `bootupd` could learn how to synchronize multiple EFI system partitions from a primary.
