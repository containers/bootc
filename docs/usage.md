---
nav_order: 3
---

# Managing bootc systems

## Performing offline updates via USB

In a usage scenario where the operating system update is in a fully
disconnected environment and you want to perform updates via e.g. inserting
a USB drive, one can do this by copying the desired OS container image to
e.g. an `oci` directory:

```bash
skopeo copy docker://quay.io/exampleos/myos:latest oci:/path/to/filesystem/myos.oci
```

Then once the USB device containing the `myos.oci` OCI directory is mounted
on the target, use

```bash
bootc switch --transport oci /var/mnt/usb/myos.oci
```

The above command can only be invoked once currently; thereafter, use `bootc upgrade`
as normal to fetch updates from the USB device.

This process can all be automated by creating systemd
units that look for a USB device with a specific label, mount (optionally with LUKS
for example), and then trigger the bootc upgrade.
