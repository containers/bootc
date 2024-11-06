# ostree vs OCI/Docker

Be sure to see the main [README.md](README.md) which describes the current architecture intersecting ostree and OCI.

Looking at this project, one might ask: why even have ostree?  Why not just have the operating system directly use something like the [containers/image](https://github.com/containers/image/) storage?

The first answer to this is that it's a goal of this project to "hide" ostree usage; it should feel "native" to ship and manage the operating system "as if" it was just running a container.

But, ostree has a *lot* of stuff built up around it and we can't just throw that away.

## Understanding kernels

ostree was designed from the start to manage bootable operating system trees - hence the name of the project.  For example, ostree understands bootloaders and kernels/initramfs images.  Container tools don't.

## Signing

ostree also quite early on gained an opinionated mechanism to sign images (commits) via GPG.  As of this time there are multiple competing mechanisms for container signing, and it is not widely deployed.
For running random containers from `docker.io`, it can be OK to just trust TLS or pin via `@sha256` - a whole idea of Docker is that containers are isolated and it should be reasonably safe to
at least try out random containers.  But for the *operating system* its integrity is paramount because it's ultimately trusted.

## Deduplication

ostree's hardlink store is designed around de-duplication.  Operating systems can get large and they are most natural as "base images" - which in the Docker container model
are duplicated on disk.  Of course storage systems like containers/image could learn to de-duplicate; but it would be a use case that *mostly* applied to just the operating system.

## Being able to remove all container images

In Kubernetes, the kubelet will prune the image storage periodically, removing images not backed by containers.  If we store the operating system itself as an image...well, we'd need to do something like teach the container storage to have the concept of an image that is "pinned" because it's actually the booted filesystem.  Or create a "fake" container representing the running operating system.

Other projects in this space ended up having an "early docker" distinct from the "main docker" which brings its own large set of challenges.

## SELinux 

OSTree has *first class* support for SELinux.  It was baked into the design from the very start.  Handling SELinux is very tricky because it's a part of the operating system that can influence *everything else*.  And specifically file labels.

In this approach we aren't trying to inject xattrs into the tar stream; they're stored out of band for reliability.

## Independence of complexity of container storage

This stuff could be done - but the container storage and tooling is already quite complex, and introducing a special case like this would be treading into new ground.

Today for example, cri-o ships a `crio-wipe.service` which removes all container storage across major version upgrades.

ostree is a fairly simple format and has been 100% stable throughout its life so far.

## ostree format has per-file integrity

More on this here: https://ostreedev.github.io/ostree/related-projects/#docker

## Allow hiding ostree while not reinventing everything

So, again the goal here is: make it feel "native" to ship and manage the operating system "as if" it was just running a container without throwing away everything in ostree today.


### Future: Running an ostree-container as a webserver

It also should work to run the ostree-container as a webserver, which will expose a webserver that responds to `GET /repo`.

The effect will be as if it was built from a `Dockerfile` that contains `EXPOSE 8080`; it will work to e.g.
`kubectl run nginx --image=quay.io/exampleos/exampleos:latest --replicas=1`
and then also create a service for it.

### Integrating with future container deltas

See https://blogs.gnome.org/alexl/2020/05/13/putting-container-updates-on-a-diet/
