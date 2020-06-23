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