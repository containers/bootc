# ostree-ext

Extension APIs for [ostree](https://github.com/ostreedev/ostree/) that are written in Rust, using the [Rust ostree bindings](https://crates.io/crates/ostree).

## module "tar": tar export/import

ostree's support for exporting to a tarball is lossy by default.  This adds a new export
format that is effectively a new custom repository mode combined with a hardlinked checkout.

This new export stream can be losslessly imported back into a different repository.

### Filesystem layout

```
.
├── etc                # content is at traditional /etc, not /usr/etc
│   └── passwd
├── sysroot       
│   └── ostree         # ostree object store with hardlinks to destinations
│       ├── repo
│       │   └── objects
│       │       ├── 00
│       │       └── 8b
│       │           └── 7df143d91c716ecfa5fc1730022f6b421b05cedee8fd52b1fc65a96030ad52.file.xattrs
│       │           └── 7df143d91c716ecfa5fc1730022f6b421b05cedee8fd52b1fc65a96030ad52.file
│       └── xattrs    # A new directory with extended attributes, hardlinked with .xattr files
│           └── 58d523efd29244331392770befa2f8bd55b3ef594532d3b8dbf94b70dc72e674
└── usr
    ├── bin
    │   └── bash
    └── lib64
        └── libc.so
```

Think of this like a new ostree repository mode `tar-stream` or so, although right now it only holds a single commit.

A major distinction is the addition of special `.xattr` files; tar variants and support library differ too much for us to rely on this making it through round trips.  And further, to support the webserver-in-container we need e.g. `security.selinux` to not be changed/overwritten by the container runtime.

## module "diff": Compute the difference between two ostree commits

```rust
    let subdir: Option<&str> = None;
    let refname = "fedora/coreos/x86_64/stable";
    let diff = ostree_ext::diff::diff(repo, &format!("{}^", refname), refname, subdir)?;
```

This is used by `rpm-ostree ex apply-live`.

## module "container": Encapsulate ostree commits in OCI/Docker images


### Bundle an OSTree repository into an OCI container directory

Given an OSTree repository, running *outside* a container:

```
$ ostree-ext-cli container export-oci --repo=/path/to/repo exampleos/x86_64/stable /output/exampleos
```
You can then e.g.

```
$ skopeo copy oci:/output/exampleos containers-storage:localhost/exampleos
$ podman run --rm -ti --entrypoint bash localhost/exampleos
```

You can also use e.g. `skopeo copy oci:/output/exampleos docker://quay.io/exampleos/exampleos:latest`.

### Future: Running an ostree-container as a webserver

It also should work to run the ostree-container as a webserver, which will expose a webserver that responds to `GET /repo`.

The effect will be as if it was built from a `Dockerfile` that contains `EXPOSE 8080`; it will work to e.g.
`kubectl run nginx --image=quay.io/exampleos/exampleos:latest --replicas=1`
and then also create a service for it.

### Pulling an ostree-container directly

A primary goal of this effort is to make it fully native to an ostree-based operating system to pull a container image directly too.

This project will hence provide a CLI tool and a Rust library which speaks the Docker/OCI protocols enough to directly pull the container image, extracting it into the system `/ostree/repo` repository.

An important aspect of this is that the system will validate the GPG signature of the target OSTree commit, as well as validating the sha256 of the contained objects.

```
$ ostree-ext-cli container import --repo=/ostree/repo quay.io/exampleos/exampleos:stable
```

A project like rpm-ostree could hence support:

```
$ rpm-ostree rebase quay.io/exampleos/exampleos:stable
```
(Along with the usual `rpm-ostree upgrade` knowing to pull that container image)

### Integrating with future container deltas

See https://blogs.gnome.org/alexl/2020/05/13/putting-container-updates-on-a-diet/


# ostree vs OCI/Docker

Looking at this, one might ask: why even have ostree?  Why not just have the operating system directly use something like the [containers/image](https://github.com/containers/image/) storage?

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

In Kubernetes, the kubelet will prune the image storage periodically, removing images not backed by containers.  If we store the operating system itself as an image...well, we'd
need to do something like teach the container storage to have the concept of an image that is "pinned" because it's actually the booted filesystem.  Or create a "fake" container
representing the running operating system.

Other projects in this space ended up having an "early docker" distinct from 

## Independence of complexity of container storage

This stuff could be done - but the container storage and tooling is already quite complex, and introducing a special case like this would be treading into new ground.

Today for example, cri-o ships a `crio-wipe.service` which removes all container storage across major version upgrades.

ostree is a fairly simple format and has been 100% stable throughout its life so far.

## ostree format has per-file integrity

More on this here: https://ostreedev.github.io/ostree/related-projects/#docker

## Allow hiding ostree while not reinventing everything

So, again the goal here is: make it feel "native" to ship and manage the operating system "as if" it was just running a container without throwing away everything in ostree today.



