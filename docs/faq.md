---
nav_order: 4
---

# Frequently Asked Questions

## How do users include their own packages/binaries in a custom "bootc compatible" container?

The "bootc compatible" containers are OCI container images, so you can customize them in the same way you build containers today.

For example, using your own yum/dnf repo in a Dockerfile:

```Dockerfile
FROM quay.io/redhat/rhel-base:10
COPY custom.repo /etc/yum.repos.d/
RUN dnf -y install custom-rpm & \
    dnf clean all && \
    ostree container commit
```

Or using multi-stage builds in your Dockerfile:

```Dockerfile
# Build a small Go program
FROM registry.access.redhat.com/ubi8/ubi:latest as builder
WORKDIR /build
COPY . .
RUN yum -y install go-toolset
RUN go build hello-world.go

FROM quay.io/redhat/rhel-base:10
COPY --from=builder /build/hello-world /usr/bin
RUN ostree container commit
```

You can find more examples at the [centos-boot-layered repo](https://github.com/CentOS/centos-boot-layered) repo or the [CoreOS layering-examples repo](https://github.com/coreos/layering-examples).

## How does the use of OCI artifacts intersect with this effort?

The "bootc compatible" images are OCI container images; they do not rely on the [OCI artifact specification](https://github.com/opencontainers/image-spec/blob/main/artifacts-guidance.md) or [OCI referrers API](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#enabling-the-referrers-api).

It is foreseeable that users will need to produce "traditional" disk images (i.e. raw disk images, qcow2 disk images, Amazon AMIs, etc.) from the "bootc compatible" container images using additional tools. Therefore, it is reasonable that some users may want to encapsulate those disk images as an OCI artifact for storage and distribution. However, it is not a goal to use `bootc` to produce these "traditional" disk images nor to facilitate the encapsulation of those disk images as OCI artifacts.
