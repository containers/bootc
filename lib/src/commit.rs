/// This module contains the functions to implement the commit
/// procedures as part of building an ostree container image.
/// https://github.com/ostreedev/ostree-rs-ext/issues/159
use anyhow::Context;
use anyhow::Result;
use std::fs;
use std::path::Path;
use tokio::task;

use crate::container_utils::is_ostree_container;

/// Check if there are any files that are not directories and error out if
/// we find any, /var should not contain any files to commit in a container
/// as it is where we expect user data to reside.
fn validate_directories_only(path: &Path, error_count: &mut i32) -> Result<()> {
    let context = || format!("Validating file: {:?}", path);
    for entry in fs::read_dir(path).with_context(context)? {
        let entry = entry?;
        let path = entry.path();

        let metadata = path.symlink_metadata()?;

        if metadata.is_dir() {
            validate_directories_only(&path, error_count)?;
        } else {
            *error_count += 1;
            if *error_count < 20 {
                eprintln!("Found file: {:?}", path)
            }
        }
    }
    Ok(())
}

/// Entrypoint to the commit procedures, initially we just
/// have one validation but we expect more in the future.
pub(crate) async fn container_commit() -> Result<()> {
    if is_ostree_container()? {
        println!("Checking /var for files");
        let var_path = Path::new("/var");

        let mut error_count = 0;

        task::spawn_blocking(move || -> Result<()> {
            validate_directories_only(var_path, &mut error_count)
        })
        .await??;

        if error_count != 0 {
            anyhow::bail!("Found content in /var");
        }
    } else {
        anyhow::bail!("Not a container can't commit");
    }
    Ok(())
}
