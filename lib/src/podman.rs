use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::install::run_in_host_mountns;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Inspect {
    pub(crate) digest: String,
}

/// Given an image ID, return its manifest digest
pub(crate) fn imageid_to_digest(imgid: &str) -> Result<String> {
    let o = run_in_host_mountns("podman")
        .args(["inspect", imgid])
        .output()?;
    let st = o.status;
    if !st.success() {
        anyhow::bail!("Failed to execute podman inspect: {st:?}");
    }
    let o: Vec<Inspect> = serde_json::from_slice(&o.stdout)?;
    let i = o
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No images returned for inspect"))?;
    Ok(i.digest)
}
