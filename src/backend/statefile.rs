//! On-disk saved state.

use crate::model::SavedState;
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use openat_ext::OpenatDirExt;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;

impl SavedState {
    /// System-wide bootupd write lock (relative to sysroot).
    const WRITE_LOCK_PATH: &'static str = "run/bootupd-lock";
    /// Top-level directory for statefile (relative to sysroot).
    pub(crate) const STATEFILE_DIR: &'static str = "boot";
    /// On-disk bootloader statefile, akin to a tiny rpm/dpkg database, stored in `/boot`.
    pub(crate) const STATEFILE_NAME: &'static str = "bootupd-state.json";

    /// Try to acquire a system-wide lock to ensure non-conflicting state updates.
    ///
    /// While ordinarily the daemon runs as a systemd unit (which implicitly
    /// ensures a single instance) this is a double check against other
    /// execution paths.
    pub(crate) fn acquire_write_lock(sysroot: openat::Dir) -> Result<StateLockGuard> {
        let lockfile = sysroot.write_file(Self::WRITE_LOCK_PATH, 0o644)?;
        lockfile.lock_exclusive()?;
        let guard = StateLockGuard {
            sysroot,
            lockfile: Some(lockfile),
        };
        Ok(guard)
    }

    /// Use this for cases when the target root isn't booted, which is
    /// offline installs.
    pub(crate) fn unlocked(sysroot: openat::Dir) -> Result<StateLockGuard> {
        Ok(StateLockGuard {
            sysroot,
            lockfile: None,
        })
    }

    /// Load the JSON file containing on-disk state.
    pub(crate) fn load_from_disk(root_path: impl AsRef<Path>) -> Result<Option<SavedState>> {
        let root_path = root_path.as_ref();
        let sysroot = openat::Dir::open(root_path)
            .with_context(|| format!("opening sysroot '{}'", root_path.display()))?;

        let statefile_path = Path::new(Self::STATEFILE_DIR).join(Self::STATEFILE_NAME);
        let saved_state = if let Some(statusf) = sysroot.open_file_optional(&statefile_path)? {
            let bufr = std::io::BufReader::new(statusf);
            let saved_state: SavedState = serde_json::from_reader(bufr)?;
            Some(saved_state)
        } else {
            None
        };
        Ok(saved_state)
    }

    /// Check whether statefile exists.
    pub(crate) fn ensure_not_present(root_path: impl AsRef<Path>) -> Result<()> {
        let statepath = Path::new(root_path.as_ref())
            .join(Self::STATEFILE_DIR)
            .join(Self::STATEFILE_NAME);
        if statepath.exists() {
            bail!("{} already exists", statepath.display());
        }
        Ok(())
    }
}

/// Write-lock guard for statefile, protecting against concurrent state updates.
#[derive(Debug)]
pub(crate) struct StateLockGuard {
    sysroot: openat::Dir,
    lockfile: Option<File>,
}

impl StateLockGuard {
    /// Atomically replace the on-disk state with a new version.
    pub(crate) fn update_state(&mut self, state: &SavedState) -> Result<()> {
        let subdir = self.sysroot.sub_dir(SavedState::STATEFILE_DIR)?;
        let f = {
            let f = subdir
                .new_unnamed_file(0o644)
                .context("creating temp file")?;
            let mut buff = std::io::BufWriter::new(f);
            serde_json::to_writer(&mut buff, state)?;
            buff.flush()?;
            buff.into_inner()?
        };
        let dest_tmp_name = {
            let mut buf = std::ffi::OsString::from(SavedState::STATEFILE_NAME);
            buf.push(".tmp");
            buf
        };
        let dest_tmp_name = Path::new(&dest_tmp_name);
        if subdir.exists(dest_tmp_name)? {
            subdir
                .remove_file(dest_tmp_name)
                .context("Removing temp file")?;
        }
        subdir
            .link_file_at(&f, dest_tmp_name)
            .context("Linking temp file")?;
        f.sync_all().context("syncing")?;
        subdir
            .local_rename(dest_tmp_name, SavedState::STATEFILE_NAME)
            .context("Renaming temp file")?;
        Ok(())
    }
}
