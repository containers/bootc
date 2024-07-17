//! Helpers for interacting with mountpoints

use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
use fn_error_context::context;
use serde::Deserialize;

use crate::task::Task;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct Filesystem {
    // Note if you add an entry to this list, you need to change the --output invocation below too
    pub(crate) source: String,
    pub(crate) target: String,
    #[serde(rename = "maj:min")]
    pub(crate) maj_min: String,
    pub(crate) fstype: String,
    pub(crate) options: String,
    pub(crate) uuid: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Findmnt {
    pub(crate) filesystems: Vec<Filesystem>,
}

fn run_findmnt(args: &[&str], path: &str) -> Result<Filesystem> {
    let desc = format!("Inspecting {path}");
    let o = Task::new(desc, "findmnt")
        .args([
            "-J",
            "-v",
            // If you change this you probably also want to change the Filesystem struct above
            "--output=SOURCE,TARGET,MAJ:MIN,FSTYPE,OPTIONS,UUID",
        ])
        .args(args)
        .arg(path)
        .quiet()
        .read()?;
    let o: Findmnt = serde_json::from_str(&o).context("Parsing findmnt output")?;
    o.filesystems
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("findmnt returned no data for {path}"))
}

#[context("Inspecting filesystem {path}")]
/// Inspect a target which must be a mountpoint root - it is an error
/// if the target is not the mount root.
pub(crate) fn inspect_filesystem(path: &Utf8Path) -> Result<Filesystem> {
    run_findmnt(&["--mountpoint"], path.as_str())
}

#[context("Inspecting filesystem by UUID {uuid}")]
/// Inspect a filesystem by partition UUID
pub(crate) fn inspect_filesystem_by_uuid(uuid: &str) -> Result<Filesystem> {
    run_findmnt(&["--source"], &(format!("UUID={uuid}")))
}

/// Mount a device to the target path.
pub(crate) fn mount(dev: &str, target: &Utf8Path) -> Result<()> {
    Task::new_and_run(
        format!("Mounting {target}"),
        "mount",
        [dev, target.as_str()],
    )
}

/// If the fsid of the passed path matches the fsid of the same path rooted
/// at /proc/1/root, it is assumed that these are indeed the same mounted
/// filesystem between container and host.
/// Path should be absolute.
#[context("Comparing filesystems at {path} and /proc/1/root/{path}")]
pub(crate) fn is_same_as_host(path: &Utf8Path) -> Result<bool> {
    // Add a leading '/' in case a relative path is passed
    let path = Utf8Path::new("/").join(path);

    // Using statvfs instead of fs, since rustix will translate the fsid field
    // for us.
    let devstat = rustix::fs::statvfs(path.as_std_path())?;
    let hostpath = Utf8Path::new("/proc/1/root").join(path.strip_prefix("/")?);
    let hostdevstat = rustix::fs::statvfs(hostpath.as_std_path())?;
    tracing::trace!(
        "base mount id {:?}, host mount id {:?}",
        devstat.f_fsid,
        hostdevstat.f_fsid
    );
    Ok(devstat.f_fsid == hostdevstat.f_fsid)
}
