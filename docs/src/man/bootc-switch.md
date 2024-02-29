# NAME

bootc-switch - Target a new container image reference to boot

# SYNOPSIS

**bootc-switch** \[**\--quiet**\] \[**\--transport**\]
\[**\--enforce-container-sigpolicy**\] \[**\--ostree-remote**\]
\[**\--retain**\] \[**-h**\|**\--help**\] \[**-V**\|**\--version**\]
\<*TARGET*\>

# DESCRIPTION

Target a new container image reference to boot.

This operates in a very similar fashion to \`upgrade\`, but changes the
container image reference instead.

# OPTIONS

**\--quiet**

:   Dont display progress

**\--transport**=*TRANSPORT* \[default: registry\]

:   The transport; e.g. oci, oci-archive. Defaults to \`registry\`

**\--enforce-container-sigpolicy**

:   This is the inverse of the previous
    \`\--target-no-signature-verification\` (which is now a no-op).

Enabling this option enforces that \`/etc/containers/policy.json\`
includes a default policy which requires signatures.

**\--ostree-remote**=*OSTREE_REMOTE*

:   Enable verification via an ostree remote

**\--retain**

:   Retain reference to currently booted image

**-h**, **\--help**

:   Print help (see a summary with -h)

**-V**, **\--version**

:   Print version

\<*TARGET*\>

:   Target image to use for the next boot

# VERSION

v0.1.0
