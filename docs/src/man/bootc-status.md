# NAME

bootc-status - Display status

# SYNOPSIS

**bootc status** \[**\--format**\] \[**\--format-version**\]
\[**\--booted**\] \[**-h**\|**\--help**\]

# DESCRIPTION

Display status

This will output a YAML-formatted object using a schema intended to
match a Kubernetes resource that describes the state of the booted
system.

The exact API format is not currently declared stable.

# OPTIONS

**\--format**=*FORMAT*

:   The output format\

\
*Possible values:*

> -   yaml: Output in YAML format
>
> -   json: Output in JSON format

**\--format-version**=*FORMAT_VERSION*

:   The desired format version. There is currently one supported
    version, which is version \`0\`. Pass this option to explicitly
    request it; it is possible that multiple versions will be supported
    in the future

**\--booted**

:   Only display status for the booted deployment

**-h**, **\--help**

:   Print help (see a summary with -h)

# VERSION

v0.1.13
