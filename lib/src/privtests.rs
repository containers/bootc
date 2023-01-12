use std::process::Command;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::rustix;
use rustix::fd::AsFd;
use xshell::{cmd, Shell};

use super::cli::TestingOpts;

const IMGSIZE: u64 = 20 * 1024 * 1024 * 1024;

struct LoopbackDevice {
    #[allow(dead_code)]
    tmpf: tempfile::NamedTempFile,
    dev: Utf8PathBuf,
}

impl LoopbackDevice {
    fn new_temp(sh: &xshell::Shell) -> Result<Self> {
        let mut tmpd = tempfile::NamedTempFile::new_in("/var/tmp")?;
        rustix::fs::ftruncate(tmpd.as_file_mut().as_fd(), IMGSIZE)?;
        let diskpath = tmpd.path();
        let path = cmd!(sh, "losetup --find --show {diskpath}").read()?;
        Ok(Self {
            tmpf: tmpd,
            dev: path.into(),
        })
    }
}

impl Drop for LoopbackDevice {
    fn drop(&mut self) {
        let _ = Command::new("losetup")
            .args(["-d", self.dev.as_str()])
            .status();
    }
}

fn init_ostree(sh: &Shell, rootfs: &Utf8Path) -> Result<()> {
    cmd!(sh, "ostree admin init-fs --modern {rootfs}").run()?;
    Ok(())
}

pub(crate) fn impl_run() -> Result<()> {
    let sh = Shell::new()?;

    let loopdev = LoopbackDevice::new_temp(&sh)?;
    let devpath = &loopdev.dev;
    println!("Using {devpath:?}");

    let td = tempfile::tempdir()?;
    let td = td.path();
    let td: &Utf8Path = td.try_into()?;

    cmd!(sh, "mkfs.xfs {devpath}").run()?;

    cmd!(sh, "mount {devpath} {td}").run()?;

    init_ostree(&sh, td)?;

    let _g = sh.push_env("OSTREE_SYSROOT", td);
    cmd!(sh, "bootc status").run()?;

    Ok(())
}

pub(crate) async fn run(opts: &TestingOpts) -> Result<()> {
    match opts {
        TestingOpts::RunPrivilegedIntegration {} => tokio::task::spawn_blocking(impl_run).await?,
    }
}
