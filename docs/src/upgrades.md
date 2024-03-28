# Managing upgrades

Right now, bootc is a quite simple tool that is designed to do just
a few things well.  One of those is transactionally fetching new operating system
updates from a registry and booting into them, while supporting rollback.

## The `bootc upgrade` verb

This will query the registry and queue an updated container image for the next boot.

This is backed today by ostree, implementing an A/B style upgrade system.
Changes to the base image are staged, and the running system is not
changed by default.

Use `bootc upgrade --apply` to auto-apply if there are queued changes.

There is also an opinionated `bootc-fetch-apply-updates.timer` and corresponding
service available in upstream for operating systems and distributions
to enable.

Man page: [bootc-upgrade](man/bootc-upgrade.md).

## Changing the container image source

Another useful pattern to implement can be to use a management agent
to invoke `bootc switch` (or declaratively via `bootc edit`)
to implement e.g. blue/green deployments,
where some hosts are rolled onto a new image independently of others.

```shell
bootc switch quay.io/examplecorp/os-prod-blue:latest
```

`bootc switch` has the same effect as `bootc upgrade`; there is no
semantic difference between the two other than changing the
container image being tracked.

This will preserve existing state in `/etc` and `/var` - for example,
host SSH keys and home directories.

Man page: [bootc-switch](man/bootc-switch.md).

## Rollback

There is a  `bootc rollback` verb, and associated declarative interface
accessible to tools via `bootc edit`.  This will swap the bootloader
ordering to the previous boot entry.

Man page: [bootc-rollback](man/bootc-rollback.md).


