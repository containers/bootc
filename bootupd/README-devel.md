# Developing bootupd

Currently the focus is Fedora CoreOS.

You can use the normal Rust tools to build and run the unit tests:

`cargo build` and `cargo test`

For real e2e testing, use e.g.
```
export COSA_DIR=/path/to/fcos
cosa build-fast
kola run -E $(pwd) --qemu-image fastbuild-fedora-coreos-bootupd-qemu.qcow2  --qemu-firmware uefi ext.bootupd.*
```

See also [the coreos-assembler docs](https://coreos.github.io/coreos-assembler/working/#using-overrides).

## Building With Containers

Many folks use a pet container or toolbox to do development on immutable, partially mutabable, or non-Linux OS's. For those who don't use a pet/toolbox and you'd prefer not to modify your host system for development you can use the `build-in-container` make target to execute building inside a container.

```
$ make build-in-container
podman build -t bootupd-build -f Dockerfile.build
STEP 1: FROM registry.fedoraproject.org/fedora:latest
STEP 2: VOLUME /srv/bootupd
--> Using cache a033bf0e43d560e72d7187459d7fad65ab30a1d01c576e8257194d82836472f7
STEP 3: WORKDIR /srv/bootupd
--> Using cache 756114416fb4a68e72b68a2097c57d9cb94c830f5b351401319baeafa062695e
STEP 4: RUN dnf update -y &&     dnf install -y make cargo rust glib2-devel openssl-devel ostree-devel
--> Using cache a8e2b525ff0701f735e01bb5703c63bb0e67683625093d34be34bf1123a7f954
STEP 5: COMMIT bootupd-build
--> a8e2b525ff0
a8e2b525ff0701f735e01bb5703c63bb0e67683625093d34be34bf1123a7f954
podman run -ti --rm -v .:/srv/bootupd:z localhost/bootupd-build make
cargo build --release
    Updating git repository `https://gitlab.com/cgwalters/ostree-rs`
    Updating crates.io index
[...]
$ ls target/release/bootupd
target/release/bootupd
$
```

## Integrating bootupd into a distribution/OS

Today, bootupd only really works on systems that use RPMs and ostree.
(Which usually means rpm-ostree, but not strictly necessarily)

Many bootupd developers (and current CI flows) target Fedora CoreOS
and derivatives, so it can be used as a "reference" for integration.

There's two parts to integration:

### Generating an update payload

Bootupd's concept of an "update payload" needs to be generated as
part of an OS image (e.g. ostree commit).  
A good reference for this is 
https://github.com/coreos/fedora-coreos-config/blob/88af117d1d2c5e828e5e039adfa03c7cc66fc733/manifests/bootupd.yaml#L12

Specifically, you'll need to invoke
`bootupctl backend generate-update-metadata /` as part of update payload generation.
This scrapes metadata (e.g. RPM versions) about shim/grub and puts them along with
their component files in `/usr/lib/bootupd/updates/`.

### Installing to generated disk images

In order to correctly manage updates, bootupd also needs to be responsible
for laying out files in initial disk images.  A good reference for this is
https://github.com/coreos/coreos-assembler/blob/93efb63dcbd63dc04a782e2c6c617ae0cd4a51c8/src/create_disk.sh#L401

Specifically, you'll need to invoke
`/usr/bin/bootupctl backend install --src-root /path/to/ostree/deploy /sysroot`
where the first path is an ostree deployment root, and the second is the physical
root partition.

This will e.g. inject the initial files into the mounted EFI system partition.
