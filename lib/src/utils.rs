use std::os::unix::prelude::OsStringExt;
use std::process::Command;

use anyhow::{Context, Result};
use ostree::glib;
use ostree_ext::container::SignatureSource;
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

/// Convert a combination of values (likely from CLI parsing) into a signature source
pub(crate) fn sigpolicy_from_opts(
    disable_verification: bool,
    ostree_remote: Option<&str>,
) -> SignatureSource {
    if disable_verification {
        SignatureSource::ContainerPolicyAllowInsecure
    } else if let Some(remote) = ostree_remote {
        SignatureSource::OstreeRemote(remote.to_owned())
    } else {
        SignatureSource::ContainerPolicy
    }
}

/// Output a warning message that we want to be quite visible.
/// The process (thread) execution will be delayed for a short time.
pub(crate) fn medium_visibility_warning(s: &str) {
    anstream::eprintln!(
        "{}{s}{}",
        anstyle::AnsiColor::Red.render_fg(),
        anstyle::Reset.render()
    );
    // When warning, add a sleep to ensure it's seen
    std::thread::sleep(std::time::Duration::from_secs(1));
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

#[test]
fn test_sigpolicy_from_opts() {
    assert_eq!(
        sigpolicy_from_opts(false, None),
        SignatureSource::ContainerPolicy
    );
    assert_eq!(
        sigpolicy_from_opts(true, None),
        SignatureSource::ContainerPolicyAllowInsecure
    );
    assert_eq!(
        sigpolicy_from_opts(false, Some("foo")),
        SignatureSource::OstreeRemote("foo".to_owned())
    );
    assert_eq!(
        sigpolicy_from_opts(true, Some("foo")),
        SignatureSource::ContainerPolicyAllowInsecure
    );
}
