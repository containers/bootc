# NAME

bootc - Deploy and transactionally in-place with bootable container
images

# SYNOPSIS

**bootc** \[**-h**\|**\--help**\] \<*subcommands*\>

# DESCRIPTION

Deploy and transactionally in-place with bootable container images.

The \`bootc\` project currently uses ostree-containers as a backend to
support a model of bootable container images. Once installed, whether
directly via \`bootc install\` (executed as part of a container) or via
another mechanism such as an OS installer tool, further updates can be
pulled via e.g. \`bootc upgrade\`.

Changes in \`/etc\` and \`/var\` persist.

# OPTIONS

**-h**, **\--help**

:   Print help (see a summary with -h)

# SUBCOMMANDS

bootc-upgrade(8)

:   Look for updates to the booted container image

bootc-switch(8)

:   Target a new container image reference to boot

bootc-edit(8)

:   Change host specification

bootc-status(8)

:   Display status

bootc-usr-overlay(8)

:   Add a transient writable overlayfs on \`/usr\` that will be
    discarded on reboot

bootc-install(8)

:   Install the running container to a target

bootc-help(8)

:   Print this message or the help of the given subcommand(s)
