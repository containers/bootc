# bootupd: Distribution-independent updates for bootloaders

Today many Linux systems handle updates for bootloader data
in an inconsistent and ad-hoc way.  For example, on
Fedora and Debian, a package manager update will update UEFI
binaries in `/boot/efi`, but not the BIOS MBR data.

Transactional/"image" update systems like [OSTree](https://github.com/ostreedev/ostree/)
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

## More details on rationale and integration

A notable problem today for [rpm-ostree](https://github.com/coreos/rpm-ostree/) based
systems is that `rpm -q shim-x64` is misleading because it's not actually
updated in place.

Particularly [this commit][1] makes things clear - the data
from the RPM goes into `/usr` (part of the OSTree), so it doesn't touch `/boot/efi`.
But that commit didn't change how the RPM database works (and more generally it
would be technically complex for rpm-ostree to change how the RPM database works today).

What we ultimately want is that `rpm -q shim-x64` returns "not installed" - because
it's not managed by RPM or by ostree.  Instead one would purely use `bootupctl` to manage it.
However, it might still be *built* as an RPM, just not installed that way. The RPM version numbers would be used
for the bootupd version associated with the payload, and ultimately we'd teach `rpm-ostree compose tree`
how to separately download bootloaders and pass them to `bootupctl backend`.

[1]: https://github.com/coreos/rpm-ostree/pull/969/commits/dc0e8db5bd92e1f478a0763d1a02b48e57022b59


## Questions and answers

- Why is bootupd not part of ostree?

This question is really covered at the top of this file already - hopefully
you read that already, but this answer will rephrase and try to go in more depth.

A key advertised feature of ostree is that updates are truly transactional.
There's even a [a test case](https://blog.verbum.org/2020/12/01/committed-to-the-integrity-of-your-root-filesystem/)
that validates forcibly pulling the power during OS updates.  A simple
way to look at this is that on an ostree-based system there is no need
to have a "please don't power off your computer" screen.  This in turn
helps administrators to confidently enable automatic updates.

Doing that for the bootloader (i.e. bootupd's domain) is an *entirely* separate problem.
There have been some ideas around how we could make the bootloaders
use an A/B type scheme (or at least be more resilient), and perhaps in the future bootupd will
use some of those.

These updates hence carry different levels of risk.  In many cases
actually it's OK if the bootloader lags behind; we don't need to update
every time.

But out of conservatism currently today for e.g. Fedora CoreOS, bootupd is disabled
by default.  On the other hand, if your OS update mechanism isn't transactional,
then you may want to enable bootupd by default.

