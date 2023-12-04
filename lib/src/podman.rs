use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::hostexec::run_in_host_mountns;
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
