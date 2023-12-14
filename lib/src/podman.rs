//! # Helpers for interacting with podman
//!
//! Wrapper for podman which writes to a bootc-owned root.

use std::os::unix::process::CommandExt;
use std::path::Path;

use anyhow::{anyhow, Result};
use camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std;
use cap_std_ext::cap_std::fs::Dir;
use ostree_ext::container::OstreeImageReference;
use serde::Deserialize;
use tokio::process::Command;

use crate::hostexec::run_in_host_mountns;
use crate::ostree_authfile;
use crate::spec::ImageReference;
use crate::utils::{cmd_in_root, newline_trim_vec_to_string};

/// The argument for podman --root, in parallel to `ostree/repo`.
pub(crate) const STORAGE_ROOT: &str = "ostree/container-storage";
/// The argument for podman --runroot, this is stored under /run/bootc.
pub(crate) const RUN_ROOT: &str = "run/bootc/container-storage";
const PODMAN_ARGS: &[&str] = &["--root", STORAGE_ROOT, "--runroot", RUN_ROOT];

pub(crate) fn podman_in_root(rootfs: &Dir) -> Result<Command> {
    let mut cmd = cmd_in_root(rootfs, "podman")?;
    cmd.args(PODMAN_ARGS);
    Ok(cmd)
}

pub(crate) async fn temporary_container_for_image(rootfs: &Dir, imageid: &str) -> Result<String> {
    tracing::debug!("Creating temporary container for {imageid}");
    let st = podman_in_root(rootfs)?
        .args(["create", imageid])
        .output()
        .await?;
    if !st.status.success() {
        anyhow::bail!("Failed to create transient image: {st:?}");
    }
    Ok(newline_trim_vec_to_string(st.stdout)?)
}

pub(crate) async fn podman_mount(rootfs: &Dir, cid: &str) -> Result<Utf8PathBuf> {
    tracing::debug!("Mounting {cid}");
    let st = podman_in_root(rootfs)?
        .args(["mount", cid])
        .output()
        .await?;
    if !st.status.success() {
        anyhow::bail!("Failed to mount transient image: {st:?}");
    }
    Ok(newline_trim_vec_to_string(st.stdout)?.into())
}

pub(crate) async fn podman_pull(
    rootfs: &Dir,
    image: &ImageReference,
    quiet: bool,
) -> Result<String> {
    let authfile = ostree_authfile::get_global_authfile_path()?;
    let mut cmd = podman_in_root(rootfs)?;
    let image = OstreeImageReference::from(image.clone());
    let pull_spec_image = image.imgref.to_string();
    tracing::debug!("Pulling {pull_spec_image}");
    let child = cmd
        .args(["pull"])
        .args(authfile.iter().flat_map(|v| [Path::new("--authfile"), v]))
        .args(quiet.then_some("--quiet"))
        .arg(&pull_spec_image)
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        anyhow::bail!("Failed to pull: {:?}", output.status);
    }
    Ok(newline_trim_vec_to_string(output.stdout)?.into())
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct PodmanInspect {
    #[allow(dead_code)]
    pub(crate) id: String,
    pub(crate) digest: String,
    pub(crate) created: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) config: PodmanInspectConfig,
    #[serde(rename = "RootFS")]
    #[allow(dead_code)]
    pub(crate) root_fs: PodmanInspectRootfs,
    pub(crate) graph_driver: PodmanInspectGraphDriver,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct PodmanInspectConfig {
    #[serde(default)]
    pub(crate) labels: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct PodmanInspectGraphDriver {
    pub(crate) name: String,
    pub(crate) data: PodmanInspectGraphDriverData,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct PodmanInspectGraphDriverData {
    pub(crate) lower_dir: String,
    pub(crate) upper_dir: String,
}

impl PodmanInspectGraphDriverData {
    pub(crate) fn layers(&self) -> impl Iterator<Item = &str> {
        self.lower_dir
            .split(':')
            .chain(std::iter::once(self.upper_dir.as_str()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct PodmanInspectRootfs {
    #[allow(dead_code)]
    pub(crate) layers: Vec<String>,
}

pub(crate) async fn podman_inspect(rootfs: &Dir, imgid: &str) -> Result<PodmanInspect> {
    let st = podman_in_root(rootfs)?
        .args(["image", "inspect", imgid])
        .output()
        .await?;
    if !st.status.success() {
        anyhow::bail!("Failed to mount transient image: {st:?}");
    }
    let r: Vec<PodmanInspect> = serde_json::from_slice(&st.stdout)?;
    let r = r
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Missing output from inspect"))?;
    Ok(r)
}

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

pub(crate) fn exec(root: &Utf8Path, args: &[std::ffi::OsString]) -> Result<()> {
    let rootfs = &Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let mut cmd = crate::utils::sync_cmd_in_root(rootfs, "podman")?;
    cmd.args(PODMAN_ARGS);
    cmd.args(args);
    Err(anyhow::Error::msg(cmd.exec()))
}
