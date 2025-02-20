//! Helper functions for bootable OSTrees.

use std::path::Path;

use anyhow::Result;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use ostree::gio;
use ostree::prelude::*;

const MODULES: &str = "usr/lib/modules";
const VMLINUZ: &str = "vmlinuz";

/// Find the kernel modules directory in a bootable OSTree commit.
/// The target directory will have a `vmlinuz` file representing the kernel binary.
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
        if child.file_type() != gio::FileType::Directory {
            continue;
        }
        let childpath = e.child(child);
        let vmlinuz = childpath.child(VMLINUZ);
        if !vmlinuz.query_exists(cancellable) {
            continue;
        }
        if r.replace(childpath).is_some() {
            anyhow::bail!("Found multiple subdirectories in {}", MODULES);
        }
    }
    Ok(r)
}

fn read_dir_optional(
    d: &Dir,
    p: impl AsRef<Path>,
) -> std::io::Result<Option<cap_std::fs::ReadDir>> {
    match d.read_dir(p.as_ref()) {
        Ok(r) => Ok(Some(r)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Find the kernel modules directory in checked out directory tree.
/// The target directory will have a `vmlinuz` file representing the kernel binary.
pub fn find_kernel_dir_fs(root: &Dir) -> Result<Option<Utf8PathBuf>> {
    let mut r = None;
    let entries = match read_dir_optional(root, MODULES)? { Some(entries) => {
        entries
    } _ => {
        return Ok(None);
    }};
    for child in entries {
        let child = &child?;
        if !child.file_type()?.is_dir() {
            continue;
        }
        let name = child.file_name();
        let name = if let Some(n) = name.to_str() {
            n
        } else {
            continue;
        };
        let mut pbuf = Utf8Path::new(MODULES).to_owned();
        pbuf.push(name);
        pbuf.push(VMLINUZ);
        if !root.try_exists(&pbuf)? {
            continue;
        }
        pbuf.pop();
        if r.replace(pbuf).is_some() {
            anyhow::bail!("Found multiple subdirectories in {}", MODULES);
        }
    }
    Ok(r)
}

#[cfg(test)]
mod test {
    use super::*;
    use cap_std_ext::{cap_std, cap_tempfile};

    #[test]
    fn test_find_kernel_dir_fs() -> Result<()> {
        let td = cap_tempfile::tempdir(cap_std::ambient_authority())?;

        // Verify the empty case
        assert!(find_kernel_dir_fs(&td).unwrap().is_none());
        let moddir = Utf8Path::new("usr/lib/modules");
        td.create_dir_all(moddir)?;
        assert!(find_kernel_dir_fs(&td).unwrap().is_none());

        let kpath = moddir.join("5.12.8-32.aarch64");
        td.create_dir_all(&kpath)?;
        td.write(kpath.join("vmlinuz"), "some kernel")?;
        let kpath2 = moddir.join("5.13.7-44.aarch64");
        td.create_dir_all(&kpath2)?;
        td.write(kpath2.join("foo.ko"), "some kmod")?;

        assert_eq!(
            find_kernel_dir_fs(&td)
                .unwrap()
                .unwrap()
                .file_name()
                .unwrap(),
            kpath.file_name().unwrap()
        );

        Ok(())
    }
}
