# Installing the binary

* Fedora: [bootc is packaged](https://bodhi.fedoraproject.org/updates/?packages=bootc).
* CentOS Stream 9: There is a [COPR](https://copr.fedorainfracloud.org/coprs/rhcontainerbot/bootc/) tracking git main with binary packages.

You can also build this project like any other Rust project, e.g. `cargo build --release` from a git clone.

# Base images

Many users will be more interested in base (container) images.

For pre-built base images; any Fedora derivative already using `ostree` can be seamlessly converted into using bootc;
for example, [Fedora CoreOS](https://quay.io/repository/fedora/fedora-coreos) can be used as a
base image; you will want to also `rpm-ostree install bootc` in your image builds currently.
There are some overlaps between `bootc` and `ignition` and `zincati` however; see
[this pull request](https://github.com/coreos/fedora-coreos-docs/pull/540) for more information.

For other derivatives such as the ["Atomic desktops"](https://gitlab.com/fedora/ostree), see
discussion of [relationships](relationships.md) which particularly covers interactions with rpm-ostree.

However, bootc itself is not tied to Fedora derivatives;
[this issue](https://github.com/coreos/bootupd/issues/468) tracks the main blocker for other distributions.
