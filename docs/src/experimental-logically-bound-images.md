# Logically Bound Images

Experimental features are subject to change or removal. Please
do provide feedback on them.

Tracking issue: <https://github.com/containers/bootc/issues/128>

## About logically bound images

This experimental feature enables an association of container "app" images to a bootc system image. A similar approach to this is [physically bound](https://github.com/containers/bootc/issues/644) images. There are some trade-offs between the two approaches. Some benefits of logically bound images are:

- The bootc system image can be updated without re-downloading the app image bits.
- The app images can be updated without modifying the bootc system image, this would be especially useful for development work

## Using logically bound images

Each image is defined in a [Podman Quadlet](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html) `.image` or `.container` file. An image is selected to be bound by creating a symlink in the `/usr/lib/bootc-experimental/bound-images.d` directory pointing to a `.image` or `.container` file. With these defined, during a `bootc upgrade` or `bootc switch` the bound images defined in the new bootc image will be automatically pulled via podman.

An example Containerfile

```Dockerfile
FROM quay.io/myorg/myimage:latest

COPY ./my-app.image /usr/share/containers/systemd/my-app.image
COPY ./another-app.container /usr/share/containers/systemd/another-app.container

RUN ln -s /usr/share/containers/systemd/my-app.image /usr/lib/bootc-experimental/bound-images.d/my-app.image && \
    ln -s /usr/share/containers/systemd/my-app.image /usr/lib/bootc-experimental/bound-images.d/my-app.image
```

## Limitations

- Currently, only the Image field of a `.image` or `.container` file is used to pull the image. Any other field is ignored.
- There is no cleanup during rollback.
- Images are subject to default garbage collection semantics; e.g. a background job pruning images without a running container may prune them. They can also be manually removed via e.g. podman rmi.
