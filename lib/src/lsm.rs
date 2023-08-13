#[cfg(feature = "install")]
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
#[cfg(feature = "install")]
use gvariant::{aligned_bytes::TryAsAligned, Marker, Structure};
#[cfg(feature = "install")]
use ostree_ext::ostree;
use ostree_ext::prelude::ToVariant;

#[cfg(feature = "install")]
use crate::task::Task;

/// The mount path for selinux
#[cfg(feature = "install")]
const SELINUXFS: &str = "/sys/fs/selinux";
/// The SELinux xattr
#[cfg(feature = "install")]
const SELINUX_XATTR: &[u8] = b"security.selinux\0";

#[context("Querying selinux availability")]
pub(crate) fn selinux_enabled() -> Result<bool> {
    let filesystems = std::fs::read_to_string("/proc/filesystems")?;
    Ok(filesystems.contains("selinuxfs\n"))
}

#[context("Ensuring selinux install_t type")]
pub(crate) fn selinux_ensure_install() -> Result<()> {
    let guardenv = "_bootc_selinuxfs_mounted";
    let current = std::fs::read_to_string("/proc/self/attr/current")
        .context("Reading /proc/self/attr/current")?;
    tracing::debug!("Current security context is {current}");
    if let Some(p) = std::env::var_os(guardenv) {
        let p = Path::new(&p);
        if p.exists() {
            tracing::debug!("Removing temporary file");
            std::fs::remove_file(p).context("Removing {p:?}")?;
        } else {
            tracing::debug!("Assuming we now have a privileged (e.g. install_t) label");
        }
        return Ok(());
    }
    tracing::debug!("Copying self to temporary file for re-exec");
    // OK now, we always copy our binary to a tempfile, set its security context
    // to match that of /usr/bin/ostree, and then re-exec.  This is really a gross
    // hack; we can't always rely on https://github.com/fedora-selinux/selinux-policy/pull/1500/commits/67eb283c46d35a722636d749e5b339615fe5e7f5
    let mut tmpf = tempfile::NamedTempFile::new()?;
    let mut src = std::fs::File::open("/proc/self/exe")?;
    let meta = src.metadata()?;
    std::io::copy(&mut src, &mut tmpf).context("Copying self to tempfile for selinux re-exec")?;
    tmpf.as_file_mut()
        .set_permissions(meta.permissions())
        .context("Setting permissions of tempfile")?;
    let tmpf: Utf8PathBuf = tmpf.keep()?.1.try_into().unwrap();
    lsm_label(&tmpf, "/usr/bin/ostree".into(), false)?;
    tracing::debug!("Created {tmpf:?}");

    let mut cmd = Command::new(&tmpf);
    cmd.env(guardenv, tmpf);
    cmd.args(std::env::args_os().skip(1));
    tracing::debug!("Re-executing {cmd:?}");
    Err(anyhow::Error::msg(cmd.exec()).context("execve"))
}

/// A type which will reset SELinux back to enforcing mode when dropped.
/// This is a workaround for the deep difficulties in trying to reliably
/// gain the `mac_admin` permission (install_t).
#[cfg(feature = "install")]
#[must_use]
pub(crate) struct SetEnforceGuard;

#[cfg(feature = "install")]
impl Drop for SetEnforceGuard {
    fn drop(&mut self) {
        let _ = selinux_set_permissive(false);
    }
}

/// Try to enter the install_t domain, but if we can't do that, then
/// just setenforce 0.
#[context("Ensuring selinux install_t type")]
#[cfg(feature = "install")]
pub(crate) fn selinux_ensure_install_or_setenforce() -> Result<Option<SetEnforceGuard>> {
    selinux_ensure_install()?;
    let current = std::fs::read_to_string("/proc/self/attr/current")
        .context("Reading /proc/self/attr/current")?;
    let g = if !current.contains("install_t") {
        tracing::warn!("Failed to enter install_t; temporarily setting permissive mode");
        selinux_set_permissive(true)?;
        Some(SetEnforceGuard)
    } else {
        None
    };
    Ok(g)
}

/// Ensure that /sys/fs/selinux is mounted, and ensure we're running
/// as install_t.
#[context("Ensuring selinux mount")]
#[cfg(feature = "install")]
pub(crate) fn container_setup_selinux() -> Result<()> {
    let path = Utf8Path::new(SELINUXFS);
    if !path.join("enforce").exists() {
        if !path.exists() {
            tracing::debug!("Creating {path}");
            std::fs::create_dir(path)?;
        }
        Task::new("Mounting selinuxfs", "mount")
            .args(["selinuxfs", "-t", "selinuxfs", path.as_str()])
            .run()?;
    }
    Ok(())
}

#[context("Setting SELinux permissive mode")]
#[allow(dead_code)]
#[cfg(feature = "install")]
pub(crate) fn selinux_set_permissive(permissive: bool) -> Result<()> {
    let enforce_path = &Utf8Path::new(SELINUXFS).join("enforce");
    if !enforce_path.exists() {
        return Ok(());
    }
    let mut f = std::fs::File::options().write(true).open(enforce_path)?;
    f.write_all(if permissive { b"0" } else { b"1" })?;
    tracing::debug!("Set SELinux permissive mode");
    Ok(())
}

fn selinux_label_for_path(target: &str) -> Result<String> {
    // TODO: detect case where SELinux isn't enabled
    let o = Command::new("matchpathcon").args(["-n", target]).output()?;
    let st = o.status;
    if !st.success() {
        anyhow::bail!("matchpathcon failed: {st:?}");
    }
    let label = String::from_utf8(o.stdout)?;
    let label = label.trim();
    Ok(label.to_string())
}

// Write filesystem labels (currently just for SELinux)
#[context("Labeling {as_path}")]
pub(crate) fn lsm_label(target: &Utf8Path, as_path: &Utf8Path, recurse: bool) -> Result<()> {
    let label = selinux_label_for_path(as_path.as_str())?;
    tracing::debug!("Label for {target} is {label}");
    let st = Command::new("chcon")
        .arg("-h")
        .args(recurse.then_some("-R"))
        .args(["-h", label.as_str(), target.as_str()])
        .status()?;
    if !st.success() {
        anyhow::bail!("Failed to invoke chcon: {st:?}");
    }
    Ok(())
}

#[cfg(feature = "install")]
pub(crate) fn xattrs_have_selinux(xattrs: &ostree::glib::Variant) -> bool {
    let v = xattrs.data_as_bytes();
    let v = v.try_as_aligned().unwrap();
    let v = gvariant::gv!("a(ayay)").cast(v);
    for xattr in v.iter() {
        let k = xattr.to_tuple().0;
        if k == SELINUX_XATTR {
            return true;
        }
    }
    false
}

#[cfg(feature = "install")]
/// Given a SELinux policy and path, return a new set of extended attributes
/// including the SELinux label corresponding to that path, if any.
pub(crate) fn new_xattrs_with_selinux(
    policy: &ostree::SePolicy,
    path: &Utf8Path,
    mode: u32,
) -> Result<ostree_ext::glib::Variant> {
    let label = policy.label(path.as_str(), mode, ostree_ext::gio::Cancellable::NONE)?;
    let r = label
        .iter()
        .map(|label| (SELINUX_XATTR, label.as_bytes()))
        .collect::<Vec<_>>();
    Ok(r.to_variant())
}
