# Recommended image content

The subdirectories here are recommended to be installed alongside
bootc in `/usr/share/doc/bootc/baseimage` - they act as reference
sources of content.

- [base](base): At the current time the content here is effectively
  a hard requirement. It's not much, just an ostree configuration
  enabling composefs, plus the default `sysroot` directory (which
  may go away in the future) and the `ostree` symlink into `sysroot`.
- [dracut](dracut): Default/basic dracut configuration; at the current
  time this basically just enables ostree in the initramfs.
- [systemd](systemd): Optional configuration for systemd, currently 
  this has configuration for kernel-install enabling rpm-ostree integration.
