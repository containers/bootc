# Developing bootupd

Currently the focus is Fedora CoreOS.

You can use the normal Rust tools to build and run the unit tests:

`cargo build` and `cargo test`

For real e2e testing, use e.g.
```
export COSA_DIR=/path/to/fcos
cosa build-fast
kola run -E (pwd) --qemu-image fastbuild-fedora-coreos-bootupd-qemu.qcow2  --qemu-firmware uefi ext.bootupd
```

See also [the coreos-assembler docs](https://github.com/coreos/coreos-assembler/blob/master/README-devel.md#using-overrides).

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
