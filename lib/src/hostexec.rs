//! Run a command in the host mount namespace

use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::{Context, Result};
use camino::Utf8Path;
use fn_error_context::context;

/// Run a command in the host mount namespace
pub(crate) fn run_in_host_mountns(cmd: &str) -> Command {
    let mut c = Command::new("/proc/self/exe");
    c.args(["exec-in-host-mount-namespace", cmd]);
    c
}

#[context("Re-exec in host mountns")]
pub(crate) fn exec_in_host_mountns(args: &[std::ffi::OsString]) -> Result<()> {
    let (cmd, args) = args[1..]
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("Missing command"))?;
    let pid1mountns = std::fs::File::open("/proc/1/ns/mnt")?;
    nix::sched::setns(pid1mountns.as_fd(), nix::sched::CloneFlags::CLONE_NEWNS).context("setns")?;
    rustix::process::chdir("/")?;
    // Work around supermin doing chroot() and not pivot_root
    // https://github.com/libguestfs/supermin/blob/5230e2c3cd07e82bd6431e871e239f7056bf25ad/init/init.c#L288
    if !Utf8Path::new("/usr").try_exists()? && Utf8Path::new("/root/usr").try_exists()? {
        tracing::debug!("Using supermin workaround");
        rustix::process::chroot("/root")?;
    }
    Err(Command::new(cmd).args(args).exec())?
}
