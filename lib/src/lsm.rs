use std::process::Command;

use anyhow::Result;
use camino::Utf8Path;
use fn_error_context::context;

use crate::task::Task;

/// The mount path for selinux
const SELINUXFS: &str = "/sys/fs/selinux";

/// Ensure that /sys/fs/selinux is mounted, and if not, do so, then
/// re-execute the current process to ensure that cached process state
/// is updated.
pub(crate) fn ensure_selinux_mount() -> Result<()> {
    if Utf8Path::new(SELINUXFS).join("enforce").exists() {
        return Ok(());
    }
    Task::new("Mounting selinuxfs", "mount")
        .args(["selinuxfs", "-t", "selinuxfs", SELINUXFS])
        .run()?;
    crate::reexec::reexec_with_guardenv("_bootc_selinuxfs_mounted")
}

// Write filesystem labels (currently just for SELinux)
#[context("Labeling {as_path}")]
pub(crate) fn lsm_label(target: &Utf8Path, as_path: &Utf8Path, recurse: bool) -> Result<()> {
    // TODO: detect case where SELinux isn't enabled
    let o = Command::new("matchpathcon")
        .args(["-n", as_path.as_str()])
        .output()?;
    let st = o.status;
    if !st.success() {
        anyhow::bail!("matchpathcon failed: {st:?}");
    }
    let label = String::from_utf8(o.stdout)?;
    let label = label.trim();

    Task::new("Setting SELinux security context (chcon)", "chcon")
        .quiet()
        .args(["-h"])
        .args(recurse.then_some("-R"))
        .args(["-h", label, target.as_str()])
        .run()
}
