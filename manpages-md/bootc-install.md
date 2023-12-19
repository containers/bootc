# NAME

bootc-install - Install the running container to a target

# SYNOPSIS

**bootc-install** \[**-h**\|**\--help**\] \[**-V**\|**\--version**\]
\<*subcommands*\>

# DESCRIPTION

Install the running container to a target.

This has two main sub-commands \`to-disk\` (which expects an empty block
device) and \`to-filesystem\` which supports installation to an already
extant filesystem.

# OPTIONS

**-h**, **\--help**

:   Print help (see a summary with -h)

**-V**, **\--version**

:   Print version

# SUBCOMMANDS

bootc-install-to-disk(8)

:   Install to the target block device

bootc-install-to-filesystem(8)

:   Install to the target filesystem

bootc-install-help(8)

:   Print this message or the help of the given subcommand(s)

# VERSION

v0.1.0
