//! Fork skopeo as a subprocess

use super::Result;
use anyhow::Context;
use std::process::Stdio;
use tokio::process::Command;

/// Create a Command builder for skopeo.
pub(crate) fn new_cmd() -> tokio::process::Command {
    let mut cmd = Command::new("skopeo");
    cmd.kill_on_drop(true);
    cmd
}

/// Spawn the child process
pub(crate) fn spawn(mut cmd: Command) -> Result<tokio::process::Child> {
    let cmd = cmd.stdin(Stdio::null()).stderr(Stdio::piped());
    Ok(cmd.spawn().context("Failed to exec skopeo")?)
}
