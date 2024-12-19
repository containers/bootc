//! Helpers for interacting with mounts.

use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use anyhow::Result;

// Fix musl support
#[cfg(target_env = "gnu")]
use libc::STATX_ATTR_MOUNT_ROOT;
#[cfg(target_env = "musl")]
const STATX_ATTR_MOUNT_ROOT: libc::c_int = 0x2000;

fn is_mountpoint_impl_statx(root: BorrowedFd, path: &Path) -> Result<Option<bool>> {
    // https://github.com/systemd/systemd/blob/8fbf0a214e2fe474655b17a4b663122943b55db0/src/basic/mountpoint-util.c#L176
    use rustix::fs::{AtFlags, StatxFlags};

    // SAFETY(unwrap): We can infallibly convert an i32 into a u64.
    let mountroot_flag: u64 = STATX_ATTR_MOUNT_ROOT.try_into().unwrap();
    match rustix::fs::statx(
        root.as_fd(),
        path,
        AtFlags::NO_AUTOMOUNT | AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::empty(),
    ) {
        Ok(r) => {
            let present = (r.stx_attributes_mask & mountroot_flag) > 0;
            Ok(present.then_some(r.stx_attributes & mountroot_flag > 0))
        }
        Err(e) if e == rustix::io::Errno::NOSYS => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Check if the target path is a mount point. On older systems without
/// `statx` support or that is missing support for the `STATX_ATTR_MOUNT_ROOT`,
/// this will return `Ok(None)`.
pub fn is_mountpoint_compat(root: BorrowedFd, path: impl AsRef<Path>) -> Result<Option<bool>> {
    is_mountpoint_impl_statx(root, path.as_ref())
}

/// Check if the target path is a mount point.
pub fn is_mountpoint(root: BorrowedFd, path: impl AsRef<Path>) -> Result<bool> {
    match is_mountpoint_compat(root, path)? {
        Some(r) => Ok(r),
        None => anyhow::bail!("statx missing mountpoint support"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cap_std_ext::{cap_std, cap_tempfile};

    #[test]
    fn test_is_mountpoint() -> Result<()> {
        let root = cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
        let supported = is_mountpoint_compat(root.as_fd(), Path::new("/")).unwrap();
        match supported {
            Some(r) => assert!(r),
            // If the host doesn't support statx, ignore this for now
            None => return Ok(()),
        }
        let tmpdir = cap_tempfile::TempDir::new(cap_std::ambient_authority())?;
        assert!(!is_mountpoint_compat(tmpdir.as_fd(), Path::new("."))
            .unwrap()
            .unwrap());
        Ok(())
    }
}
