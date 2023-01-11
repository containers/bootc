use std::os::unix::process::CommandExt;

use anyhow::Result;

/// Re-execute the current process if the provided environment variable is not set.
pub(crate) fn reexec_with_guardenv(k: &str) -> Result<()> {
    if std::env::var_os(k).is_some() {
        return Ok(());
    }
    let self_exe = std::fs::read_link("/proc/self/exe")?;
    let mut cmd = std::process::Command::new("unshare");
    cmd.env(k, "1");
    cmd.args(["-m", "--"])
        .arg(self_exe)
        .args(std::env::args_os().skip(1));
    Err(cmd.exec().into())
}
