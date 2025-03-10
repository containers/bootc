# NAME

bootc-rollback - Change the bootloader entry ordering; the deployment
under \`rollback\` will be queued for the next boot, and the current
will become rollback. If there is a \`staged\` entry (an unapplied,
queued upgrade) then it will be discarded

# SYNOPSIS

**bootc rollback** \[**-h**\|**\--help**\]

# DESCRIPTION

Change the bootloader entry ordering; the deployment under \`rollback\`
will be queued for the next boot, and the current will become rollback.
If there is a \`staged\` entry (an unapplied, queued upgrade) then it
will be discarded.

Note that absent any additional control logic, if there is an active
agent doing automated upgrades (such as the default
\`bootc-fetch-apply-updates.timer\` and associated \`.service\`) the
change here may be reverted. Its recommended to only use this in concert
with an agent that is in active control.

A systemd journal message will be logged with
\`MESSAGE_ID=26f3b1eb24464d12aa5e7b544a6b5468\` in order to detect a
rollback invocation.

# OPTIONS

**-v**, **\--verbose**

:   Increase logging verbosity

    Use `-vv`, `-vvv` to increase verbosity more.

**-h**, **\--help**

:   Print help (see a summary with -h)

# VERSION

v1.1.6
