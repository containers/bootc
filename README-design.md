Overall design
---

The initial focus here is updating the [ESP](https://en.wikipedia.org/wiki/EFI_system_partition), but the overall design of bootupd contains a lot of abstraction to support different "components".

# Ideal case

In the ideal case, an OS builder uses `bootupd install` to install all bootloader data,
and thereafter it is fully (exclusively) managed by bootupd.  It would e.g. be a bug/error
for an administrator to manually invoke `grub2-install` e.g. again.

In other words, an end user system would simply invoke `bootupd update` as desired.

However, we're not in that ideal case.  Thus bootupd has the concept of "adoption" where
we start tracking the installed state as we find it.

# Handling adoption

For Fedora CoreOS, currently the `EFI/fedora/grub.cfg` file is created outside of the ostree inside `create_disk.sh`.  So we aren't including any updates for it in the OSTree.

This type of problem is exactly what bootupd should be solving.

However, we need to be very cautious in handling this because we basically can't
assume we own all of the state.  We shouldn't touch any files that we
don't know about.

