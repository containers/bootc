#[cfg(feature = "install")]
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs::{Dir, MetadataExt};
use fn_error_context::context;
#[cfg(feature = "install")]
use gvariant::{aligned_bytes::TryAsAligned, Marker, Structure};
#[cfg(feature = "install")]
use ostree_ext::ostree;
use rustix::fd::AsRawFd;

use crate::task::Task;

/// The mount path for selinux
#[cfg(feature = "install")]
const SELINUXFS: &str = "/sys/fs/selinux";
/// The SELinux xattr
#[cfg(feature = "install")]
const SELINUX_XATTR: &[u8] = b"security.selinux\0";
const SELF_CURRENT: &str = "/proc/self/attr/current";

#[context("Querying selinux availability")]
pub(crate) fn selinux_enabled() -> Result<bool> {
    let filesystems = std::fs::read_to_string("/proc/filesystems")?;
    Ok(filesystems.contains("selinuxfs\n"))
}

/// Get the current process SELinux security context
fn get_current_security_context() -> Result<String> {
    std::fs::read_to_string(SELF_CURRENT).with_context(|| format!("Reading {SELF_CURRENT}"))
}

#[context("Testing install_t")]
fn test_install_t() -> Result<bool> {
    let tmpf = tempfile::NamedTempFile::new()?;
    let st = Command::new("chcon")
        .args(["-t", "invalid_bootcinstall_testlabel_t"])
        .arg(tmpf.path())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(st.success())
}

#[context("Ensuring selinux install_t type")]
pub(crate) fn selinux_ensure_install() -> Result<bool> {
    let guardenv = "_bootc_selinuxfs_mounted";
    let current = get_current_security_context()?;
    tracing::debug!("Current security context is {current}");
    if let Some(p) = std::env::var_os(guardenv) {
        let p = Path::new(&p);
        if p.exists() {
            tracing::debug!("Removing temporary file");
            std::fs::remove_file(p).context("Removing {p:?}")?;
        } else {
            tracing::debug!("Assuming we now have a privileged (e.g. install_t) label");
        }
        return test_install_t();
    }
    if test_install_t()? {
        tracing::debug!("We have install_t");
        return Ok(true);
    }
    tracing::debug!("Lacking install_t capabilities; copying self to temporary file for re-exec");
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
    // If the process already has install_t, exit early
    // Note that this may re-exec the entire process
    if selinux_ensure_install()? {
        return Ok(None);
    }
    let g = if std::env::var_os("BOOTC_SETENFORCE0_FALLBACK").is_some() {
        tracing::warn!("Failed to enter install_t; temporarily setting permissive mode");
        selinux_set_permissive(true)?;
        Some(SetEnforceGuard)
    } else {
        let current = get_current_security_context()?;
        anyhow::bail!("Failed to enter install_t (running as {current}) - use BOOTC_SETENFORCE0_FALLBACK=1 to override");
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
    tracing::debug!(
        "Set SELinux mode: {}",
        if permissive {
            "permissive"
        } else {
            "enforcing"
        }
    );
    Ok(())
}

fn selinux_label_for_path(target: &str) -> Result<String> {
    // TODO: detect case where SELinux isn't enabled
    let label = Task::new_quiet("matchpathcon")
        .args(["-n", target])
        .read()?;
    // TODO: trim in place instead of reallocating
    Ok(label.trim().to_string())
}

fn selinux_set_one_label(
    policy: &ostree::SePolicy,
    root: &Dir,
    path: &Utf8Path,
    mode: u32,
) -> Result<()> {
    let label = policy.label(path.as_str(), mode, ostree::gio::Cancellable::NONE)?;
    if let Some(label) = label {
        let selfpath = format!("/proc/self/fd/{}", root.as_raw_fd());
        rustix::fs::lsetxattr(
            &selfpath,
            SELINUX_XATTR,
            label.as_bytes(),
            rustix::fs::XattrFlags::empty(),
        )?;
    }
    Ok(())
}

pub(crate) fn selinux_label_recurse(
    policy: &ostree::SePolicy,
    root: &Dir,
    path: &Utf8Path,
) -> Result<()> {
    let meta = root.symlink_metadata(path)?;
    selinux_set_one_label(policy, root, path, meta.mode())?;
    if meta.is_dir() {
        for ent in root.read_dir(path)? {
            let ent = ent?;
            let name = ent.file_name();
            let name = if let Some(name) = name.to_str() {
                name
            } else {
                anyhow::bail!("Invalid filename: {name:?}");
            };
            let path = path.join(name);
            selinux_label_recurse(policy, root, &path)?;
        }
    }
    Ok(())
}

// Write filesystem labels (currently just for SELinux)
#[context("Labeling {as_path}")]
pub(crate) fn lsm_label(target: &Utf8Path, as_path: &Utf8Path, recurse: bool) -> Result<()> {
    let label = selinux_label_for_path(as_path.as_str())?;
    tracing::debug!("Label for {target} is {label}");
    Task::new_quiet("chcon")
        .arg("-h")
        .args(recurse.then_some("-R"))
        .args(["-h", label.as_str(), target.as_str()])
        .run()
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
