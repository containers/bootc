# NAME

bootc-status - Display status

# SYNOPSIS

**bootc-status** \[**\--json**\] \[**\--booted**\]
\[**-h**\|**\--help**\] \[**-V**\|**\--version**\]

# DESCRIPTION

Display status

This will output a YAML-formatted object using a schema intended to
match a Kubernetes resource that describes the state of the booted
system.

The exact API format is not currently declared stable.

# OPTIONS

**\--json**

:   Output in JSON format

**\--booted**

:   Only display status for the booted deployment

**-h**, **\--help**

:   Print help (see a summary with -h)

**-V**, **\--version**

:   Print version

# VERSION

v0.1.11
