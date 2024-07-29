use anyhow::{anyhow, Result};
use camino::Utf8Path;
use cap_std_ext::cap_std::fs::Dir;
use serde::Deserialize;

use crate::install::run_in_host_mountns;
use crate::task::Task;

/// Where we look inside our container to find our own image
/// for use with `bootc install`.
pub(crate) const CONTAINER_STORAGE: &str = "/var/lib/containers";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Inspect {
    pub(crate) digest: String,
}

/// Given an image ID, return its manifest digest
pub(crate) fn imageid_to_digest(imgid: &str) -> Result<String> {
    let out = Task::new_cmd("podman inspect", run_in_host_mountns("podman"))
        .args(["inspect", imgid])
        .quiet()
        .read()?;
    let o: Vec<Inspect> = serde_json::from_str(&out)?;
    let i = o
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No images returned for inspect"))?;
    Ok(i.digest)
}

/// Return true if there is apparently an active container store at the target path.
pub(crate) fn storage_exists(root: &Dir, path: impl AsRef<Utf8Path>) -> Result<bool> {
    fn impl_storage_exists(root: &Dir, path: &Utf8Path) -> Result<bool> {
        let lock = "storage.lock";
        root.try_exists(path.join(lock)).map_err(Into::into)
    }
    impl_storage_exists(root, path.as_ref())
}

/// Return true if there is apparently an active container store in the default path
/// for the target root.
///
/// Note this does not attempt to parse the root filesystem's container storage configuration,
/// this uses a hardcoded default path.
pub(crate) fn storage_exists_default(root: &Dir) -> Result<bool> {
    storage_exists(root, CONTAINER_STORAGE.trim_start_matches('/'))
}
