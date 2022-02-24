# Release process

The release process follows the usual PR-and-review flow, allowing an external reviewer to have a final check before publishing.

In order to ease downstream packaging of Rust binaries, an archive of vendored dependencies is also provided (only relevant for offline builds).

## Requirements

This guide requires:

 * A web browser (and network connectivity)
 * `git`
 * [GPG setup][GPG setup] and personal key for signing
 * [git-evtag](https://github.com/cgwalters/git-evtag/)
 * `cargo` (suggested: latest stable toolchain from [rustup][rustup])
 * A verified account on crates.io
 * Write access to this GitHub project
 * Upload access to this project on GitHub, crates.io
 * Membership in the [Fedora CoreOS Crates Owners group](https://github.com/orgs/coreos/teams/fedora-coreos-crates-owners/members)

## Release checklist

- Prepare local branch+commit
  - [ ] `git checkout -b release`
  - [ ] Bump the version number in `Cargo.toml`.  Usually you just want to bump the patch.
  - [ ] Run `cargo build` to ensure `Cargo.lock` would be updated
  - [ ] Commit changes `git commit -a -m 'Release x.y.z'`; include some useful brief changelog.

- Prepare the release
  - [ ] Run `./ci/prepare-release.sh`

- Validate that `origin` points to the canonical upstream repository and not your fork:
  `git remote show origin` should not be `github.com/$yourusername/$project` but should
  be under the organization ownership.  The remote `yourname` should be for your fork.

- open and merge a PR for this release:
  - [ ] `git push --set-upstream origin release`
  - [ ] open a web browser and create a PR for the branch above
  - [ ] make sure the resulting PR contains the commit
  - [ ] in the PR body, write a short changelog with relevant changes since last release
  - [ ] get the PR reviewed, approved and merged

- publish the artifacts (tag and crate):
  - [ ] `git fetch origin && git checkout ${RELEASE_COMMIT}`
  - [ ] verify `Cargo.toml` has the expected version
  - [ ] `git-evtag sign v${RELEASE_VER}`
  - [ ] `git push --tags origin v${RELEASE_VER}`
  - [ ] `cargo publish`

- publish this release on GitHub:
  - [ ] find the new tag in the [GitHub tag list](https://github.com/coreos/bootupd/tags), click the triple dots menu, and create a release for it
  - [ ] write a short changelog (i.e. re-use the PR content)
  - [ ] upload `target/${PROJECT}-${RELEASE_VER}-vendor.tar.gz`
  - [ ] record digests of local artifacts:
    - `sha256sum target/package/${PROJECT}-${RELEASE_VER}.crate`
    - `sha256sum target/${PROJECT}-${RELEASE_VER}-vendor.tar.gz`
  - [ ] publish release

- clean up:
  - [ ] `git push origin :release`
  - [ ] `cargo clean`
  - [ ] `git checkout main`

- Fedora packaging:
  - [ ] update the `rust-bootupd` spec file in [Fedora](https://src.fedoraproject.org/rpms/rust-bootupd)
    - bump the `Version`
    - switch the `Release` back to `1%{?dist}`
    - remove any patches obsoleted by the new release
    - update changelog
  - [ ] run `spectool -g -S rust-bootupd.spec`
  - [ ] run `kinit your_fas_account@FEDORAPROJECT.ORG`
  - [ ] run `fedpkg new-sources <crate-name> <vendor-tarball-name>`
  - [ ] PR the changes in [Fedora](https://src.fedoraproject.org/rpms/rust-bootupd)
  - [ ] once the PR merges to rawhide, merge rawhide into the other relevant branches (e.g. f35) then push those, for example:
    ```bash
    git checkout rawhide
    git pull --ff-only
    git checkout f35
    git merge --ff-only rawhide
    git push origin f35
    ```
  - [ ] on each of those branches run `fedpkg build`
  - [ ] once the builds have finished, submit them to [bodhi](https://bodhi.fedoraproject.org/updates/new), filling in:
    - `rust-bootupd` for `Packages`
    - selecting the build(s) that just completed, except for the rawhide one (which gets submitted automatically)
    - writing brief release notes like "New upstream release; see release notes at `link to GitHub release`"
    - leave `Update name` blank
    - `Type`, `Severity` and `Suggestion` can be left as `unspecified` unless it is a security release. In that case select `security` with the appropriate severity.
    - `Stable karma` and `Unstable` karma can be set to `2` and `-1`, respectively.
  - [ ] [submit a fast-track](https://github.com/coreos/fedora-coreos-config/actions/workflows/add-override.yml) for FCOS testing-devel
  - [ ] [submit a fast-track](https://github.com/coreos/fedora-coreos-config/actions/workflows/add-override.yml) for FCOS next-devel if it is [open](https://github.com/coreos/fedora-coreos-pipeline/blob/main/next-devel/README.md)

- RHCOS packaging:
  - [ ] update the `rust-bootupd` spec file
    - bump the `Version`
    - switch the `Release` back to `1%{?dist}`
    - remove any patches obsoleted by the new release
    - update changelog
  - [ ] run `spectool -g -S rust-bootupd.spec`
  - [ ] run `kinit your_account@REDHAT.COM`
  - [ ] run `rhpkg new-sources <crate-name> <vendor-tarball-name>`
  - [ ] PR the changes
  - [ ] get the PR reviewed and merge it
  - [ ] update your local repo and run `rhpkg build`

CentOS Stream 9 packaging:
  - [ ] to be written

[rustup]: https://rustup.rs/
[crates-io]: https://crates.io/
[GPG setup]: https://docs.github.com/en/github/authenticating-to-github/managing-commit-signature-verification
