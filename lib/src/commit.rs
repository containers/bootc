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
use std::convert::TryInto;
use std::path::Path;
use tokio::task;

/// Directories for which we will always remove all content.
const FORCE_CLEAN_PATHS: &[&str] = &["run", "tmp", "var/tmp", "var/cache"];

/// Gather count of non-empty directories.  Empty directories are removed.
fn process_dir_recurse(root: &Dir, path: &Utf8Path, error_count: &mut i32) -> Result<bool> {
    let context = || format!("Validating: {path}");
    let mut validated = true;
    for entry in root.read_dir(path).with_context(context)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = Path::new(&name);
        let name: &Utf8Path = name.try_into()?;
        let path = &path.join(name);

        let metadata = root.symlink_metadata(path)?;

        if metadata.is_dir() {
            if !process_dir_recurse(root, path, error_count)? {
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

/// Given a root filesystem, clean out empty directories and warn about
/// files in /var.  /run, /tmp, and /var/tmp have their contents recursively cleaned.
fn prepare_ostree_commit_in(root: &Dir) -> Result<()> {
    let mut error_count = 0;
    for path in FORCE_CLEAN_PATHS {
        if let Some(subdir) = root.open_dir_optional(path)? {
            for entry in subdir.entries()? {
                let entry = entry?;
                subdir.remove_all_optional(entry.file_name())?;
            }
        }
    }
    let var = Utf8Path::new("var");
    if root.try_exists(var)? && !process_dir_recurse(root, var, &mut error_count)? {
        anyhow::bail!("Found content in {var}");
    }
    Ok(())
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

        let nested = Utf8Path::new("var/lib/nested");
        td.create_dir_all(&nested)?;
        td.write(nested.join("foo"), "test1")?;
        td.write(nested.join("foo2"), "test2")?;
        assert!(prepare_ostree_commit_in(td).is_err());
        assert!(td.try_exists(var)?);

        Ok(())
    }
}
