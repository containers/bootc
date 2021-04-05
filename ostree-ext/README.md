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
