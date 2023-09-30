use std::os::unix::prelude::OsStringExt;
use std::process::Command;

use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std_ext::{cap_std::fs::Dir, prelude::CapStdExtCommandExt};
use fn_error_context::context;
use ostree::glib;
use ostree_ext::ostree;
use std::os::fd::AsFd;

/// Try to look for keys injected by e.g. rpm-ostree requesting machine-local
/// changes; if any are present, return `true`.
pub(crate) fn origin_has_rpmostree_stuff(kf: &glib::KeyFile) -> bool {
    // These are groups set in https://github.com/coreos/rpm-ostree/blob/27f72dce4f9b5c176ad030911c12354e2498c07d/rust/src/origin.rs#L23
    // TODO: Add some notion of "owner" into origin files
    for group in ["rpmostree", "packages", "overrides", "modules"] {
        if kf.has_group(group) {
            return true;
        }
    }
    false
}

/// Given an mount option string list like foo,bar=baz,something=else,ro parse it and find
/// the first entry like $optname=
/// This will not match a bare `optname` without an equals.
pub(crate) fn find_mount_option<'a>(
    option_string_list: &'a str,
    optname: &'_ str,
) -> Option<&'a str> {
    option_string_list
        .split(',')
        .filter_map(|k| k.split_once('='))
        .filter_map(|(k, v)| (k == optname).then_some(v))
        .next()
}

/// Try to (heuristically) determine if the provided path is a mount root.
pub(crate) fn is_mountpoint(root: &Dir, path: &Utf8Path) -> Result<Option<bool>> {
    // https://github.com/systemd/systemd/blob/8fbf0a214e2fe474655b17a4b663122943b55db0/src/basic/mountpoint-util.c#L176
    use rustix::fs::{AtFlags, StatxFlags};

    // SAFETY(unwrap): We can infallibly convert an i32 into a u64.
    let mountroot_flag: u64 = libc::STATX_ATTR_MOUNT_ROOT.try_into().unwrap();
    match rustix::fs::statx(
        root.as_fd(),
        path.as_std_path(),
        AtFlags::NO_AUTOMOUNT | AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::empty(),
    ) {
        Ok(r) => {
            let present = (r.stx_attributes_mask & mountroot_flag) > 0;
            Ok(present.then(|| r.stx_attributes & mountroot_flag > 0))
        }
        Err(e) if e == rustix::io::Errno::NOSYS => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Given a target directory, if it's a read-only mount, then remount it writable
#[context("Opening {target} with writable mount")]
pub(crate) fn open_dir_remount_rw(root: &Dir, target: &Utf8Path) -> Result<Dir> {
    if is_mountpoint(root, target)?.unwrap_or_default() {
        tracing::debug!("Target {target} is a mountpoint, remounting rw");
        let st = Command::new("mount")
            .args(["-o", "remount,rw", target.as_str()])
            .cwd_dir(root.try_clone()?)
            .status()?;
        if !st.success() {
            anyhow::bail!("Failed to remount: {st:?}");
        }
    }
    root.open_dir(target).map_err(anyhow::Error::new)
}

/// Run a command in the host mount namespace
#[allow(dead_code)]
pub(crate) fn run_in_host_mountns(cmd: &str) -> Command {
    let mut c = Command::new("nsenter");
    c.args(["-m", "-t", "1", "--", cmd]);
    c
}

pub(crate) fn spawn_editor(tmpf: &tempfile::NamedTempFile) -> Result<()> {
    let v = "EDITOR";
    let editor = std::env::var_os(v)
        .ok_or_else(|| anyhow::anyhow!("{v} is unset"))?
        .into_vec();
    let editor = String::from_utf8(editor).with_context(|| format!("{v} is invalid UTF-8"))?;
    let mut editor_args = editor.split_ascii_whitespace();
    let argv0 = editor_args
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid {v}: {editor}"))?;
    let status = Command::new(argv0)
        .args(editor_args)
        .arg(tmpf.path())
        .status()?;
    if !status.success() {
        anyhow::bail!("Invoking {v}: {editor} failed: {status:?}");
    }
    Ok(())
}

/// Given a possibly tagged image like quay.io/foo/bar:latest and a digest 0ab32..., return
/// the digested form quay.io/foo/bar:latest@sha256:0ab32...
/// If the image already has a digest, it will be replaced.
#[allow(dead_code)]
pub(crate) fn digested_pullspec(image: &str, digest: &str) -> String {
    let image = image.rsplit_once('@').map(|v| v.0).unwrap_or(image);
    format!("{image}@{digest}")
}

#[test]
fn test_digested_pullspec() {
    let digest = "ebe3bdccc041864e5a485f1e755e242535c3b83d110c0357fe57f110b73b143e";
    assert_eq!(
        digested_pullspec("quay.io/example/foo:bar", digest),
        format!("quay.io/example/foo:bar@{digest}")
    );
    assert_eq!(
        digested_pullspec("quay.io/example/foo@sha256:otherdigest", digest),
        format!("quay.io/example/foo@{digest}")
    );
    assert_eq!(
        digested_pullspec("quay.io/example/foo", digest),
        format!("quay.io/example/foo@{digest}")
    );
}

#[test]
fn test_find_mount_option() {
    const V1: &str = "rw,relatime,compress=foo,subvol=blah,fast";
    assert_eq!(find_mount_option(V1, "subvol").unwrap(), "blah");
    assert_eq!(find_mount_option(V1, "rw"), None);
    assert_eq!(find_mount_option(V1, "somethingelse"), None);
}
