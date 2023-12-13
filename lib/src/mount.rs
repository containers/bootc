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
    let desc = format!("Inspecting {path}");
    let o = Task::new(&desc, "findmnt")
        .args([
            "-J",
            "-v",
            // If you change this you probably also want to change the Filesystem struct above
            "--output=SOURCE,FSTYPE,OPTIONS,UUID",
            path.as_str(),
        ])
        .quiet()
        .read()?;
    let o: Findmnt = serde_json::from_str(&o).context("Parsing findmnt output")?;
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
