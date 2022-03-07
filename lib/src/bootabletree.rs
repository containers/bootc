//! Helper functions for bootable OSTrees.

use anyhow::Result;
use ostree::gio;
use ostree::prelude::*;

const MODULES: &str = "/usr/lib/modules";

/// Find the kernel modules directory in a bootable OSTree commit.
pub fn find_kernel_dir(
    root: &gio::File,
    cancellable: Option<&gio::Cancellable>,
) -> Result<Option<gio::File>> {
    let moddir = root.resolve_relative_path(MODULES);
    let e = moddir.enumerate_children(
        "standard::name",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        cancellable,
    )?;
    let mut r = None;
    for child in e.clone() {
        let child = &child?;
        let childpath = e.child(child);
        if child.file_type() == gio::FileType::Directory && r.replace(childpath).is_some() {
            anyhow::bail!("Found multiple subdirectories in {}", MODULES);
        }
    }
    Ok(r)
}
