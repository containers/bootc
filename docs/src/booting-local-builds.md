# Booting local builds

In some scenarios, you may want to boot a *locally* built
container image, in order to apply a persistent hotfix
to a specific server, or as part of a development/testing
scenario.

## Building a new local image

At the current time, the bootc host container storage is distinct
from that of the `podman` container runtime storage (default
configuration in `/var/lib/containers`).

It not currently streamlined to export the booted host container
storage into the podman storage.

Hence today, to replicate the exact container image the
host has booted, take the container image referenced
in `bootc status` and turn it into a `podman pull`
invocation.

Next, craft a container build file with your desired changes:
```
FROM <image>
RUN apt|dnf upgrade https://example.com/systemd-hotfix.package
```

## Copying an updated image into the bootc storage

This command is straightforward; we just need to tell bootc
to fetch updates from `containers-storage`, which is the
local "application" container runtime (podman) storage:

```
$ bootc switch --transport containers-storage quay.io/fedora/fedora-bootc:40
```

From there, the new image will be queued for the next boot
and a `reboot` will apply it.

For more on valid transports, see [containers-transports](https://github.com/containers/image/blob/main/docs/containers-transports.5.md).
