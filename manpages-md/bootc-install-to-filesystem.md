# NAME

bootc-install-to-filesystem - Install to the target filesystem

# SYNOPSIS

**bootc-install-to-filesystem** \[**\--root-mount-spec**\]
\[**\--root-options**\] \[**\--boot-mount-spec**\] \[**\--replace**\]
\[**\--source-imgref**\] \[**\--target-transport**\]
\[**\--target-imgref**\] \[**\--enforce-container-sigpolicy**\]
\[**\--target-ostree-remote**\] \[**\--skip-fetch-check**\]
\[**\--disable-selinux**\] \[**\--karg**\] \[**\--generic-image**\]
\[**-h**\|**\--help**\] \[**-V**\|**\--version**\] \<*ROOT_PATH*\>

# DESCRIPTION

Install to the target filesystem

# OPTIONS

**\--root-mount-spec**=*ROOT_MOUNT_SPEC*

:   Source device specification for the root filesystem. For example,
    UUID=2e9f4241-229b-4202-8429-62d2302382e1

**\--root-options**=*ROOT_OPTIONS*

:   Comma-separated mount options for the root filesystem. For example:
    rw,prjquota

**\--boot-mount-spec**=*BOOT_MOUNT_SPEC*

:   Mount specification for the /boot filesystem.

At the current time, a separate /boot is required. This restriction will
be lifted in future versions. If not specified, the filesystem UUID will
be used.

**\--replace**=*REPLACE*

:   Initialize the system in-place; at the moment, only one mode for
    this is implemented. In the future, it may also be supported to set
    up an explicit \"dual boot\" system\

\
*Possible values:*

> -   wipe: Completely wipe the contents of the target filesystem. This
>     cannot be done if the target filesystem is the one the system is
>     booted from
>
> -   alongside: This is a destructive operation in the sense that the
>     bootloader state will have its contents wiped and replaced.
>     However, the running system (and all files) will remain in place
>     until reboot

**\--source-imgref**=*SOURCE_IMGREF*

:   Install the system from an explicitly given source.

By default, bootc install and install-to-filesystem assumes that it runs
in a podman container, and it takes the container image to install from
the podmans container registry. If \--source-imgref is given, bootc uses
it as the installation source, instead of the behaviour explained in the
previous paragraph. See skopeo(1) for accepted formats.

**\--target-transport**=*TARGET_TRANSPORT* \[default: registry\]

:   The transport; e.g. oci, oci-archive. Defaults to \`registry\`

**\--target-imgref**=*TARGET_IMGREF*

:   Specify the image to fetch for subsequent updates

**\--enforce-container-sigpolicy**

:   This is the inverse of the previous
    \`\--target-no-signature-verification\` (which is now a no-op).
    Enabling this option enforces that \`/etc/containers/policy.json\`
    includes a default policy which requires signatures

**\--target-ostree-remote**=*TARGET_OSTREE_REMOTE*

:   Enable verification via an ostree remote

**\--skip-fetch-check**

:   By default, the accessiblity of the target image will be verified
    (just the manifest will be fetched). Specifying this option
    suppresses the check; use this when you know the issues it might
    find are addressed.

A common reason this may fail is when one is using an image which
requires registry authentication, but not embedding the pull secret in
the image so that updates can be fetched by the installed OS \"day 2\".

**\--disable-selinux**

:   Disable SELinux in the target (installed) system.

This is currently necessary to install \*from\* a system with SELinux
disabled but where the target does have SELinux enabled.

**\--karg**=*KARG*

:   Add a kernel argument

**\--generic-image**

:   Perform configuration changes suitable for a \"generic\" disk image.
    At the moment:

\- All bootloader types will be installed - Changes to the system
firmware will be skipped

**-h**, **\--help**

:   Print help (see a summary with -h)

**-V**, **\--version**

:   Print version

\<*ROOT_PATH*\>

:   Path to the mounted root filesystem.

By default, the filesystem UUID will be discovered and used for
mounting. To override this, use \`\--root-mount-spec\`.

# VERSION

v0.1.0
