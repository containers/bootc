use std::os::fd::BorrowedFd;
use std::os::unix::prelude::OsStringExt;
use std::process::Command;

use anyhow::{Context, Result};
use cap_std_ext::{cap_std::fs::Dir, cmdext::CapStdExtCommandExt};
use ostree::glib;
use ostree_ext::ostree;

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

#[allow(unsafe_code)]
pub(crate) fn sysroot_fd_borrowed(sysroot: &ostree_ext::ostree::Sysroot) -> BorrowedFd {
    // SAFETY: Just borrowing an existing fd; there's aleady a PR to add this
    // api to libostree
    unsafe { BorrowedFd::borrow_raw(sysroot.fd()) }
}

#[allow(unsafe_code)]
fn set_pdeathsig(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: This is a straightforward use of prctl; would be good
    // to put in a crate (maybe cap-std-ext)
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::Term))
                .map_err(Into::into)
        });
    }
}

/// Create a Command instance that has its current working directory set
/// to the target root, and is also lifecycle-bound to us.
pub(crate) fn sync_cmd_in_root(rootfs: &Dir, cmd: &str) -> Result<std::process::Command> {
    let mut cmd = std::process::Command::new(cmd);
    cmd.cwd_dir(rootfs.try_clone()?);
    set_pdeathsig(&mut cmd);
    Ok(cmd)
}

/// Create a Command instance that has its current working directory set
/// to the target root, and is also lifecycle-bound to us.
pub(crate) fn cmd_in_root(rootfs: &Dir, cmd: &str) -> Result<tokio::process::Command> {
    let mut cmd = std::process::Command::new(cmd);
    cmd.cwd_dir(rootfs.try_clone()?);
    set_pdeathsig(&mut cmd);
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.kill_on_drop(true);
    Ok(cmd)
}

/// Output a warning message
pub(crate) fn warning(s: &str) {
    anstream::eprintln!(
        "{}{s}{}",
        anstyle::AnsiColor::Red.render_fg(),
        anstyle::Reset.render()
    );
}

pub(crate) fn newline_trim_vec_to_string(mut v: Vec<u8>) -> Result<String> {
    let mut i = v.len();
    while i > 0 && v[i - 1] == b'\n' {
        i -= 1;
    }
    v.truncate(i);
    String::from_utf8(v).map_err(Into::into)
}

/// Given a possibly tagged image like quay.io/foo/bar:latest and a digest 0ab32..., return
/// the digested form quay.io/foo/bar:latest@sha256:0ab32...
/// If the image already has a digest, it will be replaced.
#[allow(dead_code)]
pub(crate) fn digested_pullspec(image: &str, digest: &str) -> String {
    let image = image.rsplit_once('@').map(|v| v.0).unwrap_or(image);
    format!("{image}@{digest}")
}

#[allow(dead_code)]
pub(crate) fn require_sha256_digest(blobid: &str) -> Result<&str> {
    let r = blobid
        .split_once("sha256:")
        .ok_or_else(|| anyhow::anyhow!("Missing sha256: in blob ID: {blobid}"))?
        .1;
    if r.len() != 64 {
        anyhow::bail!("Invalid digest in blob ID: {blobid}");
    }
    if !r.chars().all(|c| char::is_ascii_alphanumeric(&c)) {
        anyhow::bail!("Invalid checksum in blob ID: {blobid}");
    }
    Ok(r)
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

#[test]
fn test_newline_trim() {
    let ident_cases = ["", "foo"].into_iter().map(|s| s.as_bytes());
    for case in ident_cases {
        let r = newline_trim_vec_to_string(Vec::from(case)).unwrap();
        assert_eq!(case, r.as_bytes());
    }
    let cases = [("foo\n", "foo"), ("bar\n\n", "bar")];
    for (orig, new) in cases {
        let r = newline_trim_vec_to_string(Vec::from(orig)).unwrap();
        assert_eq!(new.as_bytes(), r.as_bytes());
    }
}

#[test]
fn test_require_sha256_digest() {
    assert_eq!(
        require_sha256_digest(
            "sha256:0b145899261c8a62406f697c67040cbd811f4dfaa9d778426cf1953413be8534"
        )
        .unwrap(),
        "0b145899261c8a62406f697c67040cbd811f4dfaa9d778426cf1953413be8534"
    );
    for e in ["", "sha256:abcde", "sha256:0b145899261c8a62406f697c67040cbd811f4dfaa9d778426cf1953413b34🦀123", "sha512:9895de267ca908c36ed0031c017ba9bf85b83c21ff2bf241766a4037be81f947c68841ee75f003eba3b4bddc524c0357d7bc9ebffe499f5b72f2da3507cb170d"] {
        assert!(require_sha256_digest(e).is_err());
    }
}
