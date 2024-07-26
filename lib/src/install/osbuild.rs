//! # Helper APIs for interacting with bootc-image-builder
//!
//! See <https://github.com/osbuild/bootc-image-builder>
//!

use anyhow::Result;
use camino::Utf8Path;
use cap_std_ext::{cap_std::fs::Dir, cmdext::CapStdExtCommandExt};
use fn_error_context::context;

use crate::task::Task;

/// Handle /etc/containers readonly mount.
///
/// Ufortunately today podman requires that /etc be writable for
/// `/etc/containers/networks`. bib today creates this as a readonly mount:
/// https://github.com/osbuild/osbuild/blob/4edbe227d41c767441b9bf4390398afc6dc8f901/osbuild/buildroot.py#L243
///
/// Work around that by adding a transient, writable overlayfs.
fn adjust_etc_containers(tempdir: &Dir) -> Result<()> {
    let etc_containers = Utf8Path::new("/etc/containers");
    // If there's no /etc/containers, nothing to do
    if !etc_containers.try_exists()? {
        return Ok(());
    }
    if rustix::fs::access(etc_containers.as_std_path(), rustix::fs::Access::WRITE_OK).is_ok() {
        return Ok(());
    }
    // Create dirs for the overlayfs upper and work in the install-global tmpdir.
    tempdir.create_dir_all("etc-ovl/upper")?;
    tempdir.create_dir("etc-ovl/work")?;
    let opts = format!("lowerdir={etc_containers},workdir=etc-ovl/work,upperdir=etc-ovl/upper");
    let mut t = Task::new(
        &format!("Mount transient overlayfs for {etc_containers}"),
        "mount",
    )
    .args(["-t", "overlay", "overlay", "-o", opts.as_str()])
    .arg(etc_containers);
    t.cmd.cwd_dir(tempdir.try_clone()?);
    t.run()?;
    Ok(())
}

/// bootc-image-builder today does a few things that we need to
/// deal with.
#[context("bootc-image-builder adjustments")]
pub(crate) fn adjust_for_bootc_image_builder(tempdir: &Dir) -> Result<()> {
    adjust_etc_containers(tempdir)?;
    Ok(())
}
