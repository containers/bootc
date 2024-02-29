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

## Changing the container image source

Another useful pattern to implement can be to use a management agent
to invoke `bootc switch` to implement e.g. blue/green deployments,
where some hosts are rolled onto a new image independently of others.

```shell
bootc switch quay.io/examplecorp/os-prod-blue:lastest
```

This will preserve existing state in `/etc` and `/var` - for example,
host SSH keys and home directories.

## Rollback

At the current time, bootc does not ship with an opinionated integrated
rollback flow.  However, bootc always maintains (by default) a
`rollback` container image that is accessible via `bootc status`.
