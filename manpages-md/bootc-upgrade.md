# NAME

bootc-upgrade - Look for updates to the booted container image

# SYNOPSIS

**bootc-upgrade** \[**\--quiet**\] \[**\--touch-if-changed**\]
\[**\--check**\] \[**\--apply**\] \[**-h**\|**\--help**\]
\[**-V**\|**\--version**\]

# DESCRIPTION

Look for updates to the booted container image

# OPTIONS

**\--quiet**

:   Dont display progress

**\--touch-if-changed**=*TOUCH_IF_CHANGED*

:   

**\--check**

:   Check if an update is available without applying it

**\--apply**

:   Restart or reboot into the new target image.

Currently, this option always reboots. In the future this command will
detect the case where no kernel changes are queued, and perform a
userspace-only restart.

**-h**, **\--help**

:   Print help (see a summary with -h)

**-V**, **\--version**

:   Print version

# VERSION

v0.1.0
