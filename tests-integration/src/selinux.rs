use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cap_std_ext::cap_std::fs::Dir;

fn verify_selinux_label_exists(root: &Dir, path: &Path, warn: bool) -> Result<()> {
    let mut buf = [0u8; 1024];
    let fdpath = format!("/proc/self/fd/{}/", root.as_raw_fd());
    let fdpath = &Path::new(&fdpath).join(path);
    match rustix::fs::lgetxattr(fdpath, "security.selinux", &mut buf) {
        // Ignore EOPNOTSUPPORTED
        Ok(_) | Err(rustix::io::Errno::OPNOTSUPP) => Ok(()),
        Err(rustix::io::Errno::NODATA) if warn => {
            eprintln!("No SELinux label found for: {path:?}");
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("Failed to look up context for {path:?}")),
    }
}

pub(crate) fn verify_selinux_recurse(root: &Dir, path: &mut PathBuf, warn: bool) -> Result<()> {
    for ent in root.read_dir(&path)? {
        let ent = ent?;
        let name = ent.file_name();
        path.push(name);
        verify_selinux_label_exists(root, &path, warn)?;
        let file_type = ent.file_type()?;
        if file_type.is_dir() {
            verify_selinux_recurse(root, path, warn)?;
        }
        path.pop();
    }
    Ok(())
}
