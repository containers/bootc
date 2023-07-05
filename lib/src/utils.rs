use std::os::unix::prelude::OsStringExt;
use std::process::Command;

use anyhow::{Context, Result};
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
