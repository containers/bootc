//! Helpers for interacting with mountpoints

use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
use fn_error_context::context;
use serde::Deserialize;
use std::fmt;

use crate::task::Task;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct Filesystem {
    // Note if you add an entry to this list, you need to change the --output invocation below too
    pub(crate) source: String,
    pub(crate) fstype: String,
    pub(crate) options: String,
    pub(crate) uuid: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(crate) enum FilesystemType {
    DevTmpFs,
}

impl fmt::Display for FilesystemType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            FilesystemType::DevTmpFs => write!(f, "devtmpfs"),
        }
    }
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
            "--output=SOURCE,FSTYPE,OPTIONS,UUID",
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

/// Mount a device to the target path.
pub(crate) fn mount(dev: &str, target: &Utf8Path) -> Result<()> {
    Task::new_and_run(
        format!("Mounting {target}"),
        "mount",
        [dev, target.as_str()],
    )
}

/// Create the target directory if it does not exist, then mount the specified filesystem
pub(crate) fn ensure_mount(dev: &str, target: &Utf8Path, fstype: FilesystemType) -> Result<()> {
    std::fs::create_dir_all(target)?;
    Task::new(format!("Mounting {fstype} {target}"), "mount")
        .args(["-t", format!("{fstype}").as_str(), dev, target.as_str()])
        .quiet()
        .run()?;
    Ok(())
}
