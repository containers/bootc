# Logically Bound Images

Experimental features are subject to change or removal. Please
do provide feedback on them.

Tracking issue: <https://github.com/containers/bootc/issues/128>

## About logically bound images

This experimental feature enables an association of container "app" images to a base bootc system image. Use cases for this include:

- Logging (e.g. journald->remote log forwarder container)
- Monitoring (e.g. [Prometheus node_exporter](https://github.com/prometheus/node_exporter))
- Configuration management agents
- Security agents

These types of things are commonly not updated outside of the host, and there's a secondary important property: We *always* want them present and available on the host, possibly from very early on in the boot. In contrast with default usage of tools like `podman` or `docker`, images may be pulled dynamically *after* the boot starts; requiring functioning networking, etc. For example if the remote registry is unavailable temporarily, the host system may run for a longer period of time without log forwarding or monitoring, which can be very undesirable.

Another simple way to say this is that logically bound images allow you to reference container images with the same confidence you can with `ExecStart=` in a systemd unit.

The term "logically bound" was created to contrast with [physically bound](https://github.com/containers/bootc/issues/644) images. There are some trade-offs between the two approaches. Some benefits of logically bound images are:

- The bootc system image can be updated without re-downloading the app image bits.
- The app images can be updated without modifying the bootc system image, this would be especially useful for development work

## Using logically bound images

Each image is defined in a [Podman Quadlet](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html) `.image` or `.container` file. An image is selected to be bound by creating a symlink in the `/usr/lib/bootc/bound-images.d` directory pointing to a `.image` or `.container` file. 

With these defined, during a `bootc upgrade` or `bootc switch` the bound images defined in the new bootc image will be automatically pulled into the bootc image storage, and are available to container runtimes such as podman by explicitly configuring them to point to the bootc storage as an "additional image store", via e.g.:

`podman --storage-opt=additionalimagestore=/usr/lib/bootc/storage run <image> ...`

An example Containerfile

```Dockerfile
FROM quay.io/myorg/myimage:latest

COPY ./my-app.image /usr/share/containers/systemd/my-app.image
COPY ./another-app.container /usr/share/containers/systemd/another-app.container

RUN ln -s /usr/share/containers/systemd/my-app.image /usr/lib/bootc/bound-images.d/my-app.image && \
    ln -s /usr/share/containers/systemd/my-app.image /usr/lib/bootc/bound-images.d/my-app.image
```

In the `.container` definition, you should use:

```
GlobalArgs=--storage-opt=additionalimagestore=/usr/lib/bootc/storage
```

## Pull secret

Images are fetched using the global bootc pull secret by default (`/etc/ostree/auth.json`). It is not yet supported to configure `PullSecret` in these image definitions.

## Garbage collection

The bootc image store is owned by bootc; images will be garbage collected when they are no longer referenced
by a file in `/usr/lib/bootc/bound-images.d`.

## Installation

Logically bound images must be present in the default container store (`/var/lib/containers`) when invoking
[bootc install](bootc-install.md); the images will be copied into the target system and present
directly at boot, alongside the bootc base image.

## Limitations

The *only* field parsed and honored by bootc currently is the `Image` field of a `.image` or `.container` file.

Other pull-relevant flags such as `PullSecret=` for example are not supported (see above).
Another example unsupported flag is `Arch` (the default host architecture is always used).

There is no mechanism to inject arbitrary arguments to the `podman pull` (or equivalent)
invocation used by bootc. However, many properties used for container registry interaction
can be configured via [containers-registries.conf](https://github.com/containers/image/blob/main/docs/containers-registries.conf.5.md)
and apply to all commands operating on that image.

### Distro/OS installer support

At the current time, logically bound images are [not supported by Anaconda](https://github.com/rhinstaller/anaconda/discussions/5197).
