//! # bootc-managed container storage
//!
//! The default storage for this project uses ostree, canonically storing all of its state in
//! `/sysroot/ostree`.
//!
//! This containers-storage: which canonically lives in `/sysroot/ostree/bootc`.

use std::sync::Arc;

use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::cmdext::CapStdExtCommandExt;
use cap_std_ext::dirext::CapStdExtDirExt;
use fn_error_context::context;
use std::os::fd::OwnedFd;

use crate::task::Task;

/// The path to the storage, relative to the physical system root.
pub(crate) const SUBPATH: &str = "ostree/bootc/storage";
/// The path to the "runroot" with transient runtime state; this is
/// relative to the /run directory
const RUNROOT: &str = "bootc/storage";
pub(crate) struct Storage {
    root: Dir,
    #[allow(dead_code)]
    run: Dir,
}

impl Storage {
    fn podman_task_in(sysroot: OwnedFd, run: OwnedFd) -> Result<crate::task::Task> {
        let mut t = Task::new_quiet("podman");
        // podman expects absolute paths for these, so use /proc/self/fd
        {
            let sysroot_fd: Arc<OwnedFd> = Arc::new(sysroot);
            t.cmd.take_fd_n(sysroot_fd, 3);
        }
        {
            let run_fd: Arc<OwnedFd> = Arc::new(run);
            t.cmd.take_fd_n(run_fd, 4);
        }
        t = t.args(["--root=/proc/self/fd/3", "--runroot=/proc/self/fd/4"]);
        Ok(t)
    }

    #[allow(dead_code)]
    fn podman_task(&self) -> Result<crate::task::Task> {
        let sysroot = self.root.try_clone()?.into_std_file().into();
        let run = self.run.try_clone()?.into_std_file().into();
        Self::podman_task_in(sysroot, run)
    }

    #[context("Creating imgstorage")]
    pub(crate) fn create(sysroot: &Dir, run: &Dir) -> Result<Self> {
        let subpath = Utf8Path::new(SUBPATH);
        // SAFETY: We know there's a parent
        let parent = subpath.parent().unwrap();
        if !sysroot.try_exists(subpath)? {
            let tmp = format!("{SUBPATH}.tmp");
            sysroot.remove_all_optional(&tmp)?;
            sysroot.create_dir_all(parent)?;
            sysroot.create_dir_all(&tmp).context("Creating tmpdir")?;
            // There's no explicit API to initialize a containers-storage:
            // root, simply passing a path will attempt to auto-create it.
            // We run "podman images" in the new root.
            Self::podman_task_in(sysroot.open_dir(&tmp)?.into(), run.try_clone()?.into())?
                .arg("images")
                .run()?;
            sysroot
                .rename(&tmp, sysroot, subpath)
                .context("Renaming tmpdir")?;
        }
        Self::open(sysroot, run)
    }

    #[context("Opening imgstorage")]
    pub(crate) fn open(sysroot: &Dir, run: &Dir) -> Result<Self> {
        let root = sysroot.open_dir(SUBPATH).context(SUBPATH)?;
        // Always auto-create this if missing
        run.create_dir_all(RUNROOT)?;
        let run = run.open_dir(RUNROOT).context(RUNROOT)?;
        Ok(Self { root, run })
    }
}
