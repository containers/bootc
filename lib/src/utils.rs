use std::future::Future;
use std::io::Write;
use std::os::fd::BorrowedFd;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs::Dir;
use ostree::glib;
use ostree_ext::container::SignatureSource;
use ostree_ext::ostree;

/// Helpers intended for [`std::process::Command`].
pub(crate) trait CommandRunExt {
    fn run(&mut self) -> Result<()>;
}

impl CommandRunExt for Command {
    /// Synchronously execute the child, and return an error if the child exited unsuccessfully.
    fn run(&mut self) -> Result<()> {
        let st = self.status()?;
        if !st.success() {
            // Note that we intentionally *don't* include the command string
            // in the output; we leave it to the caller to add that if they want,
            // as it may be verbose.
            anyhow::bail!(format!("Subprocess failed: {st:?}"))
        }
        Ok(())
    }
}

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

// Access the file descriptor for a sysroot
#[allow(unsafe_code)]
pub(crate) fn sysroot_fd(sysroot: &ostree::Sysroot) -> BorrowedFd {
    unsafe { BorrowedFd::borrow_raw(sysroot.fd()) }
}

// Return a cap-std `Dir` type for a deployment.
// TODO: in the future this should perhaps actually mount via composefs
pub(crate) fn deployment_fd(
    sysroot: &ostree::Sysroot,
    deployment: &ostree::Deployment,
) -> Result<Dir> {
    let sysroot_dir = &Dir::reopen_dir(&sysroot_fd(sysroot))?;
    let dirpath = sysroot.deployment_dirpath(deployment);
    sysroot_dir.open_dir(&dirpath).map_err(Into::into)
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
    let editor_variables = ["EDITOR"];
    // These roughly match https://github.com/systemd/systemd/blob/769ca9ab557b19ee9fb5c5106995506cace4c68f/src/shared/edit-util.c#L275
    let backup_editors = ["nano", "vim", "vi"];
    let editor = editor_variables.into_iter().find_map(std::env::var_os);
    let editor = if let Some(e) = editor.as_ref() {
        e.to_str()
    } else {
        backup_editors
            .into_iter()
            .find(|v| std::path::Path::new("/usr/bin").join(v).exists())
    };
    let editor =
        editor.ok_or_else(|| anyhow::anyhow!("$EDITOR is unset, and no backup editor found"))?;
    let mut editor_args = editor.split_ascii_whitespace();
    let argv0 = editor_args
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid editor: {editor}"))?;
    let status = Command::new(argv0)
        .args(editor_args)
        .arg(tmpf.path())
        .status()
        .context("Spawning editor")?;
    if !status.success() {
        anyhow::bail!("Invoking editor: {editor} failed: {status:?}");
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

/// Call an async task function, and write a message to stdout
/// with an automatic spinner to show that we're not blocked.
/// Note that generally the called function should not output
/// anything to stdout as this will interfere with the spinner.
pub(crate) async fn async_task_with_spinner<F, T>(msg: &str, f: F) -> T
where
    F: Future<Output = T>,
{
    let pb = indicatif::ProgressBar::new_spinner();
    let style = indicatif::ProgressStyle::default_bar();
    pb.set_style(style.template("{spinner} {msg}").unwrap());
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(150));
    // We need to handle the case where we aren't connected to
    // a tty, so indicatif would show nothing by default.
    if pb.is_hidden() {
        print!("{}...", msg);
        std::io::stdout().flush().unwrap();
    }
    let r = f.await;
    if pb.is_hidden() {
        println!("done");
    } else {
        pb.finish_with_message(format!("{msg}: done"));
    }
    r
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

#[test]
fn command_run_ext() {
    Command::new("true").run().unwrap();
    assert!(Command::new("false").run().is_err());
}
