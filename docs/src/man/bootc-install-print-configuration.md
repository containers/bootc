# NAME

bootc-install-print-configuration - Output JSON to stdout that contains
the merged installation configuration as it may be relevant to calling
processes using \`install to-filesystem\` that want to honor e.g.
\`root-fs-type\`

# SYNOPSIS

**bootc install print-configuration** \[**-h**\|**\--help**\]

# DESCRIPTION

Output JSON to stdout that contains the merged installation
configuration as it may be relevant to calling processes using \`install
to-filesystem\` that want to honor e.g. \`root-fs-type\`.

At the current time, the only output key is \`root-fs-type\` which is a
string-valued filesystem name suitable for passing to \`mkfs.\$type\`.

# OPTIONS

**-h**, **\--help**

:   Print help (see a summary with -h)

# VERSION

v0.1.11
