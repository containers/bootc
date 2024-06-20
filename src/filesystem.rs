use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::{Context, Result};
use fn_error_context::context;
use rustix::fd::BorrowedFd;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
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

#[context("Inspecting filesystem {path:?}")]
pub(crate) fn inspect_filesystem(root: &openat::Dir, path: &str) -> Result<Filesystem> {
    let rootfd = unsafe { BorrowedFd::borrow_raw(root.as_raw_fd()) };
    // SAFETY: This is unsafe just for the pre_exec, when we port to cap-std we can use cap-std-ext
    let o = unsafe {
        Command::new("findmnt")
            .args(["-J", "-v", "--output=SOURCE,FSTYPE,OPTIONS,UUID", path])
            .pre_exec(move || rustix::process::fchdir(rootfd).map_err(Into::into))
            .output()?
    };
    let st = o.status;
    if !st.success() {
        let _ = std::io::stderr().write_all(&o.stderr)?;
        anyhow::bail!("findmnt failed: {st:?}");
    }
    let o: Findmnt = serde_json::from_reader(std::io::Cursor::new(&o.stdout))
        .context("Parsing findmnt output")?;
    o.filesystems
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("findmnt returned no data"))
}
