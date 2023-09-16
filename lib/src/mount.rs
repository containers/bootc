//! Helpers for interacting with mountpoints

use std::process::Command;

use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
use fn_error_context::context;
use serde::Deserialize;

use crate::task::Task;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct Filesystem {
    pub(crate) source: String,
    pub(crate) fstype: String,
    pub(crate) options: String,
    pub(crate) uuid: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Findmnt {
    pub(crate) filesystems: Vec<Filesystem>,
}

#[context("Inspecting filesystem {path}")]
pub(crate) fn inspect_filesystem(path: &Utf8Path) -> Result<Filesystem> {
    tracing::debug!("Inspecting {path}");
    let o = Command::new("findmnt")
        .args(["-J", "-v", "--output-all", path.as_str()])
        .output()?;
    let st = o.status;
    if !st.success() {
        anyhow::bail!("findmnt {path} failed: {st:?}");
    }
    let o: Findmnt = serde_json::from_reader(std::io::Cursor::new(&o.stdout))
        .context("Parsing findmnt output")?;
    o.filesystems
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("findmnt returned no data for {path}"))
}

/// Mount a device to the target path.
pub(crate) fn mount(dev: &str, target: &Utf8Path) -> Result<()> {
    Task::new_and_run(
        format!("Mounting {target}"),
        "mount",
        [dev, target.as_str()],
    )
}
