# Hacking on bootc

Thanks for your interest in contributing!  At the current time,
bootc is implemented in Rust, and calls out to important components
which are written in Go (e.g. https://github.com/containers/image)
as well as C (e.g. https://github.com/ostreedev/ostree/).  Depending
on what area you want to work on, you'll need to be familiar with
the relevant language.

There isn't a single approach to working on bootc; however
the primary developers tend to use Linux host systems,
and test in Linux VMs.  One specifically recommended
approach is to use [toolbox](https://github.com/containers/toolbox/)
to create a containerized development environment
(it's possible, though not necessary to create the toolbox
 dev environment using a bootc image as well).

At the current time most upstream developers use a Fedora derivative
as a base, and the [hack/Containerfile](hack/Containerfile) defaults
to Fedora.  However, bootc itself is not intended to strongly tie to a particular
OS or distribution, and patches to handle others are gratefully
accepted!

## Key recommended ingredients:

- A development environment (toolbox or a host) with a Rust and C compiler, etc.
  While this isn't specific to bootc, you will find the experience of working on Rust
  is greatly aided with use of e.g. [rust-analyzer](https://github.com/rust-lang/rust-analyzer/).
- An installation of [podman-bootc](https://github.com/containers/podman-bootc-cli)
  which note on Linux requires that you set up "podman machine".

## Ensure you're familiar with a bootc system

Worth stating: before you start diving into the code you should understand using
the system as a user and how it works.  See the user documentation for that.

## Creating your edit-compile-debug cycle

Edit the source code; a simple thing to do is add e.g.
`eprintln!("hello world);` into `run_from_opt` in [lib/src/cli.rs](lib/src/cli.rs).
You can run `make` or `cargo build` to build that locally.  However, a key
next step is to get that binary into a bootc container image.

Use e.g. `podman build -t localhost/bootc -f hack/Containerfile .`.

From there, you can create and spawn a VM from that container image
with your modified bootc code in exactly the same way as a systems operator
would test their own bootc images:

```
$ podman-bootc run localhost/bootc
```

### Faster iteration cycles

You don't need to create a whole new VM for each change, of course.
<https://github.com/containers/podman-bootc/pull/36> is an outstanding
PR to add virtiofsd support, which would allow easily accessing the locally-built
binaries.  Another avenue we'll likely investigate is supporting podman-bootc
accessing the container images which currently live in the podman-machine VM,
or having a local registry which frontends the built container images.

A simple hack though (assuming your development environment is compatible
with the target container host) is to just run a webserver on the host, e.g.
`python3 -m http.server` or whatever, and then from the podman-bootc guest
run `bootc usroverlay` once, and 
`curl -L -o /usr/bin/bootc http://10.0.1.2:8080/target/release/bootc && restorecon /usr/bin/bootc`.

## Running the tests

First, you can run many unit tests with `cargo test`.

### container tests

There's a small set of tests which are designed to run inside a bootc container
and are built into the default container image:

```
$ podman run --rm -ti localhost/bootc bootc-integration-tests container
```








