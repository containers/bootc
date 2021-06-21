# Questions and answers

## module "container": Encapsulate OSTree commits in OCI/Docker images

### How is this different from the "tarball-of-archive-repo" approach currently used in RHEL CoreOS? Aren't both encapsulating an OSTree commit in an OCI image?

- The "tarball-of-archive-repo" approach is essentially just putting an OSTree repo in archive mode under `/srv` as an additional layer over a regular RHEL base image. In the new data format, users can do e.g. `podman run --rm -ti quay.io/fedora/fedora-coreos:stable bash`. This could be quite useful for some tests for OSTree commits (at one point we had a test that literally booted a whole VM to run `rpm -q` - it'd be much cheaper to do those kinds of "OS sanity checks" in a container).

- The new data format is intentionally designed to be streamed; the files inside the tarball are ordered by (commit, metadata, content ...). With "tarball-of-archive-repo" as is today that's not true, so we need to pull and extract the whole thing to a temporary location, which is inefficient. See also https://github.com/ostreedev/ostree-rs-ext/issues/1.

- We have a much clearer story for adding Docker/OCI style _derivation_ later.

- The new data format abstracts away OSTree a bit more and avoids needing people to think about OSTree unnecessarily.

### Why pull from a container image instead of the current (older) method of pulling from OSTree repos?

A good example is for people who want to do offline/disconnected installations and updates. They will almost certainly have container images they want to pull too - now the OS is just another container image. Users no longer need to mirror OSTree repos. Overall, as mentioned already, we want to abstract away OSTree a bit more.

### Can users view this as a regular container image?

Yes, and it also provides some extras. In addition to being able to be run as a container, if the host is OSTree-based, the host itself can be deployed/updated into this image, too. There is also GPG signing and per-file integrity validation that comes with OSTree.

### So then would this OSTree commit in container image also work as a bootimage (bootable from a USB drive)?

No. Though there could certainly be kernels and initramfses in the (OSTree commit in the) container image, that doesn't make it bootable. OSTree _understands_ bootloaders and can update kernels/initramfs images, but it doesn't update bootloaders, that is [bootupd](https://github.com/coreos/bootupd)'s job. Furthermore, this is still a container image, made of tarballs and manifests; it is not formatted to be a disk image (e.g. it doesn't have a FAT32 formatted ESP). Related to this topic is https://github.com/iximiuz/docker-to-linux, which illustrates the difference between a docker image and a bootable image. 
TL;DR, OSTree commit in container image is meant only to deliver OS updates (OSTree commits), not bootable disk images. 

### How much deduplication do we still get with this new approach?

Unfortunately, today, we do indeed need to download more than actually needed, but the files will still be deduplicated on disk, just like before. So we still won't be storing extra files, but we will be downloading extra files.
But for users doing offline mirroring, this shouldn't matter that much. In OpenShift, the entire image is downloaded today, as well. 
Nevertheless, see https://github.com/ostreedev/ostree-rs-ext/#integrating-with-future-container-deltas.

### Will there be support for "layers" in the OSTree commit in container image?

Not yet, but, as mentioned above, this opens up the possibility of doing OCI style derivation, so this could certainly be added later. It would be useful to make this image as familiar to admins as possible. Right now, the ostree-rs-ext client is only parsing one layer of the container image.

### How will mirroring image registries work?

since ostree-rs-ext uses skopeo (which uses `containers/image`), mirroring is transparently supported, i.e. admins can configure their mirroring in `containers-registries.conf` and it'll just work.
