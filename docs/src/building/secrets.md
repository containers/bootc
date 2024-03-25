
# Secrets (e.g. container pull secrets)

To have `bootc` fetch updates from registry which requires authentication,
you must include a pull secret in `/etc/ostree/auth.json`.

Another common case is to also fetch container images via
`podman` or equivalent.  There is a [pull request to add `/etc/containers/auth.json`](https://github.com/containers/image/pull/1746)
which would be shared by the two stacks by default.

Regardless, injecting this data is a good example of a generic
"secret".  The bootc project does not currently include one
single opinionated mechanism for secrets.

## Embedding in container build

This was mentioned above; you can include secrets in
the container image if the registry server is suitably protected.

In some cases, embedding only "bootstrap" secrets into the container
image is a viable pattern, especially alongside a mechanism for
having a machine authenticate to a cluster.   In this pattern,
a provisioning tool (whether run as part of the host system
or a container image) uses the bootstrap secret to lay down
and keep updated other secrets (for example, SSH keys,
certificates).

## Via cloud metadata

Most production IaaS systems support a "metadata server" or equivalent
which can securely host secrets - particularly "bootstrap secrets".
Your container image can include tooling such as `cloud-init`
or `ignition` which fetches these secrets.

## Embedded in disk images

Another pattern is to embed bootstrap secrets only in disk images.
For example, when generating a cloud disk image (AMI, OpenStack glance image, etc.)
from an input container image, the disk image can contain secrets that
are effectively machine-local state.  Rotating them would
require an additional management tool, or refreshing disk images.

## Injected via baremetal installers

It is common for installer tools to support injecting configuration
which can commonly cover secrets like this.

## Injecting secrets via systemd credentials

The systemd project has documentation for [credentials](https://systemd.io/CREDENTIALS/)
which applies in some deployment methodologies.
