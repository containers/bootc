//! This module contains the functions to implement the commit
//! procedures as part of building an ostree container image.
//! <https://github.com/ostreedev/ostree-rs-ext/issues/159>

use crate::container_utils::require_ostree_container;
use anyhow::Context;
use anyhow::Result;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use std::fs;
use tokio::task;

/// Check if there are any files that are not directories and error out if
/// we find any, /var should not contain any files to commit in a container
/// as it is where we expect user data to reside.
fn validate_directories_only_recurse(path: &Utf8Path, error_count: &mut i32) -> Result<()> {
    let context = || format!("Validating file: {:?}", path);
    for entry in fs::read_dir(path).with_context(context)? {
        let entry = entry?;
        let path = entry.path();
        let path: Utf8PathBuf = path.try_into()?;

        let metadata = path.symlink_metadata()?;

        if metadata.is_dir() {
            validate_directories_only_recurse(&path, error_count)?;
        } else {
            *error_count += 1;
            if *error_count < 20 {
                eprintln!("Found file: {:?}", path)
            }
        }
    }
    Ok(())
}

fn validate_ostree_compatibility_in(root: &Utf8Path) -> Result<()> {
    let var_path = root.join("var");
    println!("Checking /var for files");
    let mut error_count = 0;
    validate_directories_only_recurse(&var_path, &mut error_count)?;
    if error_count != 0 {
        anyhow::bail!("Found content in {var_path}");
    }
    Ok(())
}

fn validate_ostree_compatibility() -> Result<()> {
    validate_ostree_compatibility_in(Utf8Path::new("/"))
}

/// Entrypoint to the commit procedures, initially we just
/// have one validation but we expect more in the future.
pub(crate) async fn container_commit() -> Result<()> {
    require_ostree_container()?;

    task::spawn_blocking(validate_ostree_compatibility).await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit() -> Result<()> {
        let td = tempfile::tempdir()?;
        let td = td.path();
        let td = Utf8Path::from_path(td).unwrap();

        let var = td.join("var");

        std::fs::create_dir(&var)?;
        validate_ostree_compatibility_in(td).unwrap();

        std::fs::write(var.join("foo"), "somefile")?;

        assert!(validate_ostree_compatibility_in(td).is_err());

        Ok(())
    }
}
