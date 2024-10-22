//! Helpers for interacting with mountpoints

use std::process::Command;

use anyhow::{anyhow, Result};
use bootc_utils::CommandRunExt;
use camino::Utf8Path;
use fn_error_context::context;
use serde::Deserialize;

use crate::task::Task;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
pub(crate) struct Filesystem {
    // Note if you add an entry to this list, you need to change the --output invocation below too
    pub(crate) source: String,
    pub(crate) target: String,
    #[serde(rename = "maj:min")]
    pub(crate) maj_min: String,
    pub(crate) fstype: String,
    pub(crate) options: String,
    pub(crate) uuid: Option<String>,
    pub(crate) children: Option<Vec<Filesystem>>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Findmnt {
    pub(crate) filesystems: Vec<Filesystem>,
}

fn run_findmnt(args: &[&str], path: &str) -> Result<Findmnt> {
    let o: Findmnt = Command::new("findmnt")
        .args([
            "-J",
            "-v",
            // If you change this you probably also want to change the Filesystem struct above
            "--output=SOURCE,TARGET,MAJ:MIN,FSTYPE,OPTIONS,UUID",
        ])
        .args(args)
        .arg(path)
        .log_debug()
        .run_and_parse_json()?;
    Ok(o)
}

// Retrieve a mounted filesystem from a device given a matching path
fn findmnt_filesystem(args: &[&str], path: &str) -> Result<Filesystem> {
    let o = run_findmnt(args, path)?;
    o.filesystems
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("findmnt returned no data for {path}"))
}

#[context("Inspecting filesystem {path}")]
/// Inspect a target which must be a mountpoint root - it is an error
/// if the target is not the mount root.
pub(crate) fn inspect_filesystem(path: &Utf8Path) -> Result<Filesystem> {
    findmnt_filesystem(&["--mountpoint"], path.as_str())
}

#[context("Inspecting filesystem by UUID {uuid}")]
/// Inspect a filesystem by partition UUID
pub(crate) fn inspect_filesystem_by_uuid(uuid: &str) -> Result<Filesystem> {
    findmnt_filesystem(&["--source"], &(format!("UUID={uuid}")))
}

// Check if a specified device contains an already mounted filesystem
// in the root mount namespace
pub(crate) fn is_mounted_in_pid1_mountns(path: &str) -> Result<bool> {
    let o = run_findmnt(&["-N"], "1")?;

    let mounted = o.filesystems.iter().any(|fs| is_source_mounted(path, fs));

    Ok(mounted)
}

// Recursively check a given filesystem to see if it contains an already mounted source
pub(crate) fn is_source_mounted(path: &str, mounted_fs: &Filesystem) -> bool {
    if mounted_fs.source.contains(path) {
        return true;
    }

    if let Some(ref children) = mounted_fs.children {
        for child in children {
            if is_source_mounted(path, child) {
                return true;
            }
        }
    }

    false
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
