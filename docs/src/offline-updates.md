# Mirrored/disconnected upgrades

It is common (a best practice even) to maintain systems which default
to being disconnected from the public Internet.

## Pulling updates from a local mirror

The bootc project reuses the same container libraries that are in use by `podman`;
this means that configuring [containers-registries.conf](https://github.com/containers/image/blob/main/docs/containers-registries.conf.5.md)
allows `bootc upgrade` to fetch from local mirror registries.

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

The above command is only necessary once, and thereafter will be idempotent.
Then, use `bootc upgrade --apply` to fetch and apply the update from the USB device.

This process can all be automated by creating systemd
units that look for a USB device with a specific label, mount (optionally with LUKS
for example), and then trigger the bootc upgrade.
