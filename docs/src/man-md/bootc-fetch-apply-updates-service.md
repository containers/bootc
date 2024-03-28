# man bootc-fetch-apply-updates.service

This systemd service and associated `.timer` unit simply invoke
`bootc upgrade --apply`.  It is a minimal demonstration of
an "upgrade agent".

More information: [bootc-upgrade](../man/bootc-upgrade.md).

The systemd unit is not enabled by default upstream, but it
may be enabled in some operating systems.
