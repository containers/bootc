use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::Result;
use fn_error_context::context;

/// Re-execute the current process if the provided environment variable is not set.
#[context("Reexec self")]
pub(crate) fn reexec_with_guardenv(k: &str, prefix_args: &[&str]) -> Result<()> {
    if std::env::var_os(k).is_some() {
        tracing::trace!("Skipping re-exec due to env var {k}");
        return Ok(());
    }
    let self_exe = std::fs::read_link("/proc/self/exe")?;
    let mut prefix_args = prefix_args.iter();
    let mut cmd = if let Some(p) = prefix_args.next() {
        let mut c = Command::new(p);
        c.args(prefix_args);
        c.arg(self_exe);
        c
    } else {
        Command::new(self_exe)
    };
    cmd.env(k, "1");
    cmd.args(std::env::args_os().skip(1));
    tracing::debug!("Re-executing current process for {k}");
    Err(cmd.exec().into())
}
