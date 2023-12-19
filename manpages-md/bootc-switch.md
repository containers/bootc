# NAME

bootc-switch - Target a new container image reference to boot

# SYNOPSIS

**bootc-switch** \[**\--quiet**\] \[**\--transport**\]
\[**\--no-signature-verification**\] \[**\--ostree-remote**\]
\[**\--retain**\] \[**-h**\|**\--help**\] \[**-V**\|**\--version**\]
\<*TARGET*\>

# DESCRIPTION

Target a new container image reference to boot

# OPTIONS

**\--quiet**

:   Dont display progress

**\--transport**=*TRANSPORT* \[default: registry\]

:   The transport; e.g. oci, oci-archive. Defaults to \`registry\`

**\--no-signature-verification**

:   Explicitly opt-out of requiring any form of signature verification

**\--ostree-remote**=*OSTREE_REMOTE*

:   Enable verification via an ostree remote

**\--retain**

:   Retain reference to currently booted image

**-h**, **\--help**

:   Print help

**-V**, **\--version**

:   Print version

\<*TARGET*\>

:   Target image to use for the next boot

# VERSION

v0.1.0
