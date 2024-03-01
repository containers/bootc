# Installing the binary

* Fedora: [bootc is packaged](https://bodhi.fedoraproject.org/updates/?packages=bootc).
* CentOS Stream 9: There is a [COPR](https://copr.fedorainfracloud.org/coprs/rhcontainerbot/bootc/) tracking git main with binary packages.

You can also build this project like any other Rust project, e.g. `cargo build --release` from a git clone.

# Base images

Many users will be more interested in base (container) images.

For pre-built base images:

* Any Fedora derivative already using `ostree` can be seamlessly converted into using bootc; for example, [Fedora CoreOS](https://quay.io/repository/fedora/fedora-coreos) can be used as a base image; you will want to also `rpm-ostree install bootc` in your image builds currently.
* There is also an in-development [centos-boot](https://github.com/centos/centos-boot) project.

However, bootc itself is not tied to Fedora derivatives; [this issue](https://github.com/coreos/bootupd/issues/468) tracks the main blocker for other distributions.
