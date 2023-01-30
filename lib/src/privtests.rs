use std::process::Command;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::rustix;
use fn_error_context::context;
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

#[context("bootc status")]
fn run_bootc_status() -> Result<()> {
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

    // Basic sanity test of `bootc status` on an uninitialized root
    let _g = sh.push_env("OSTREE_SYSROOT", td);
    cmd!(sh, "bootc status").run()?;

    Ok(())
}

// This needs nontrivial work for loopback devices
// #[context("bootc install")]
// fn run_bootc_install() -> Result<()> {
//     let sh = Shell::new()?;
//     let loopdev = LoopbackDevice::new_temp(&sh)?;
//     let devpath = &loopdev.dev;
//     println!("Using {devpath:?}");

//     let selinux_enabled = crate::lsm::selinux_enabled()?;
//     let selinux_opt = if selinux_enabled {
//         ""
//     } else {
//         "--disable-selinux"
//     };

//     cmd!(sh, "bootc install {selinux_opt} {devpath}").run()?;

//     Ok(())
// }

/// Tests run an ostree-based host
#[context("Privileged container tests")]
pub(crate) fn impl_run_host() -> Result<()> {
    run_bootc_status()?;
    println!("ok bootc status");
    //run_bootc_install()?;
    //println!("ok bootc install");
    println!("ok host privileged testing");
    Ok(())
}

#[context("Container tests")]
pub(crate) fn impl_run_container() -> Result<()> {
    assert!(ostree_ext::container_utils::is_ostree_container()?);
    let sh = Shell::new()?;
    let stout = cmd!(sh, "bootc status").read()?;
    assert!(stout.contains("Running in a container (ostree base)."));
    drop(stout);
    let o = Command::new("bootc").arg("upgrade").output()?;
    let st = o.status;
    assert!(!st.success());
    let stderr = String::from_utf8(o.stderr)?;
    assert!(stderr.contains("this command requires a booted host system"));
    println!("ok container integration testing");
    Ok(())
}

pub(crate) async fn run(opts: TestingOpts) -> Result<()> {
    match opts {
        TestingOpts::RunPrivilegedIntegration {} => {
            crate::cli::ensure_self_unshared_mount_namespace().await?;
            tokio::task::spawn_blocking(impl_run_host).await?
        }
        TestingOpts::RunContainerIntegration {} => {
            tokio::task::spawn_blocking(impl_run_container).await?
        }
    }
}
