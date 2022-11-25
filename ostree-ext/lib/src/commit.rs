//! This module contains the functions to implement the commit
//! procedures as part of building an ostree container image.
//! <https://github.com/ostreedev/ostree-rs-ext/issues/159>

use crate::container_utils::require_ostree_container;
use anyhow::Context;
use anyhow::Result;
use camino::Utf8Path;
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use cap_std_ext::dirext::CapStdExtDirExt;
use cap_std_ext::rustix::fs::MetadataExt;
use std::convert::TryInto;
use std::path::Path;
use std::path::PathBuf;
use tokio::task;

/// Directories for which we will always remove all content.
const FORCE_CLEAN_PATHS: &[&str] = &["run", "tmp", "var/tmp", "var/cache"];

/// Gather count of non-empty directories.  Empty directories are removed.
fn process_dir_recurse(
    root: &Dir,
    rootdev: u64,
    path: &Utf8Path,
    error_count: &mut i32,
) -> Result<bool> {
    let context = || format!("Validating: {path}");
    let mut validated = true;
    for entry in root.read_dir(path).with_context(context)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.dev() != rootdev {
            continue;
        }
        let name = entry.file_name();
        let name = Path::new(&name);
        let name: &Utf8Path = name.try_into()?;
        let path = &path.join(name);

        if metadata.is_dir() {
            if !process_dir_recurse(root, rootdev, path, error_count)? {
                validated = false;
            }
        } else {
            validated = false;
            *error_count += 1;
            if *error_count < 20 {
                eprintln!("Found file: {:?}", path)
            }
        }
    }
    if validated {
        root.remove_dir(path).with_context(context)?;
    }
    Ok(validated)
}

/// Recursively remove the target directory, but avoid traversing across mount points.
fn remove_all_on_mount_recurse(root: &Dir, rootdev: u64, path: &Path) -> Result<bool> {
    let mut skipped = false;
    for entry in root.read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.dev() != rootdev {
            skipped = true;
            continue;
        }
        let name = entry.file_name();
        let path = &path.join(name);

        if metadata.is_dir() {
            skipped |= remove_all_on_mount_recurse(root, rootdev, path.as_path())?;
        } else {
            root.remove_file(path)?;
        }
    }
    if !skipped {
        root.remove_dir(&path)?;
    }
    Ok(skipped)
}

fn clean_subdir(root: &Dir, rootdev: u64) -> Result<()> {
    for entry in root.entries()? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let dev = metadata.dev();
        // Ignore other filesystem mounts, e.g. podman injects /run/.containerenv
        if dev != rootdev {
            continue;
        }
        let path = PathBuf::from(entry.file_name());
        if metadata.is_dir() {
            remove_all_on_mount_recurse(root, rootdev, &path)?;
        } else {
            root.remove_file(&path)?;
        }
    }
    Ok(())
}

fn clean_paths_in(root: &Dir, rootdev: u64) -> Result<()> {
    for path in FORCE_CLEAN_PATHS {
        let subdir = if let Some(subdir) = root.open_dir_optional(path)? {
            subdir
        } else {
            continue;
        };
        clean_subdir(&subdir, rootdev).with_context(|| format!("Cleaning {path}"))?;
    }
    Ok(())
}

#[allow(clippy::collapsible_if)]
fn process_var(root: &Dir, rootdev: u64, strict: bool) -> Result<()> {
    let var = Utf8Path::new("var");
    let mut error_count = 0;
    if root.try_exists(var)? {
        if !process_dir_recurse(root, rootdev, var, &mut error_count)? && strict {
            anyhow::bail!("Found content in {var}");
        }
    }
    Ok(())
}

/// Given a root filesystem, clean out empty directories and warn about
/// files in /var.  /run, /tmp, and /var/tmp have their contents recursively cleaned.
pub fn prepare_ostree_commit_in(root: &Dir) -> Result<()> {
    let rootdev = root.dir_metadata()?.dev();
    clean_paths_in(root, rootdev)?;
    process_var(root, rootdev, true)
}

/// Like [`prepare_ostree_commit_in`] but only emits warnings about unsupported
/// files in `/var` and will not error.
pub fn prepare_ostree_commit_in_nonstrict(root: &Dir) -> Result<()> {
    let rootdev = root.dir_metadata()?.dev();
    clean_paths_in(root, rootdev)?;
    process_var(root, rootdev, false)
}

/// Entrypoint to the commit procedures, initially we just
/// have one validation but we expect more in the future.
pub(crate) async fn container_commit() -> Result<()> {
    task::spawn_blocking(move || {
        require_ostree_container()?;
        let rootdir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
        prepare_ostree_commit_in(&rootdir)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit() -> Result<()> {
        let td = &cap_tempfile::tempdir(cap_std::ambient_authority())?;

        // Handle the empty case
        prepare_ostree_commit_in(td).unwrap();
        prepare_ostree_commit_in_nonstrict(td).unwrap();

        let var = Utf8Path::new("var");
        let run = Utf8Path::new("run");
        let tmp = Utf8Path::new("tmp");
        let vartmp_foobar = &var.join("tmp/foo/bar");
        let runsystemd = &run.join("systemd");
        let resolvstub = &runsystemd.join("resolv.conf");

        for p in [var, run, tmp] {
            td.create_dir(p)?;
        }

        td.create_dir_all(vartmp_foobar)?;
        td.write(vartmp_foobar.join("a"), "somefile")?;
        td.write(vartmp_foobar.join("b"), "somefile2")?;
        td.create_dir_all(runsystemd)?;
        td.write(resolvstub, "stub resolv")?;
        prepare_ostree_commit_in(td).unwrap();
        assert!(!td.try_exists(var)?);
        assert!(td.try_exists(run)?);
        assert!(!td.try_exists(runsystemd)?);

        let systemd = run.join("systemd");
        td.create_dir_all(&systemd)?;
        prepare_ostree_commit_in(td).unwrap();
        assert!(!td.try_exists(var)?);

        td.create_dir(&var)?;
        td.write(var.join("foo"), "somefile")?;
        assert!(prepare_ostree_commit_in(td).is_err());
        assert!(td.try_exists(var)?);

        td.write(var.join("foo"), "somefile")?;
        prepare_ostree_commit_in_nonstrict(td).unwrap();
        assert!(td.try_exists(var)?);

        let nested = Utf8Path::new("var/lib/nested");
        td.create_dir_all(&nested)?;
        td.write(nested.join("foo"), "test1")?;
        td.write(nested.join("foo2"), "test2")?;
        assert!(prepare_ostree_commit_in(td).is_err());
        assert!(td.try_exists(var)?);

        Ok(())
    }
}