//! APIs for creating container images from OSTree commits

use super::*;
use crate::tar as ostree_tar;
use anyhow::Context;
use fn_error_context::context;
use gio::glib;
use ostree::gio;
use std::collections::BTreeMap;
use std::path::Path;
use tracing::{instrument, Level};

/// Configuration for the generated container.
#[derive(Debug, Default)]
pub struct Config {
    /// Additional labels.
    pub labels: Option<BTreeMap<String, String>>,
    /// The equivalent of a `Dockerfile`'s `CMD` instruction.
    pub cmd: Option<Vec<String>>,
}

/// Write an ostree commit to an OCI blob
#[context("Writing ostree root to blob")]
fn export_ostree_ref_to_blobdir(
    repo: &ostree::Repo,
    rev: &str,
    ocidir: &openat::Dir,
    compression: Option<flate2::Compression>,
) -> Result<oci::Layer> {
    let commit = repo.resolve_rev(rev, false)?.unwrap();
    let mut w = oci::LayerWriter::new(ocidir, compression)?;
    ostree_tar::export_commit(repo, commit.as_str(), &mut w)?;
    w.complete()
}

/// Generate an OCI image from a given ostree root
#[context("Building oci")]
fn build_oci(
    repo: &ostree::Repo,
    rev: &str,
    ocidir_path: &Path,
    config: &Config,
    compression: Option<flate2::Compression>,
) -> Result<ImageReference> {
    // Explicitly error if the target exists
    std::fs::create_dir(ocidir_path).context("Creating OCI dir")?;
    let ocidir = &openat::Dir::open(ocidir_path)?;
    let writer = &mut oci::OciWriter::new(ocidir)?;

    let commit = repo.resolve_rev(rev, false)?.unwrap();
    let commit = commit.as_str();
    let (commit_v, _) = repo.load_commit(commit)?;
    let commit_meta = &commit_v.child_value(0);
    let commit_meta = glib::VariantDict::new(Some(commit_meta));

    if let Some(version) =
        commit_meta.lookup_value("version", Some(glib::VariantTy::new("s").unwrap()))
    {
        let version = version.str().unwrap();
        writer.add_config_annotation("version", version);
        writer.add_manifest_annotation("ostree.version", version);
    }

    writer.add_config_annotation(OSTREE_COMMIT_LABEL, commit);
    writer.add_manifest_annotation(OSTREE_COMMIT_LABEL, commit);

    for (k, v) in config.labels.iter().map(|k| k.iter()).flatten() {
        writer.add_config_annotation(k, v);
    }
    if let Some(cmd) = config.cmd.as_ref() {
        let cmd: Vec<_> = cmd.iter().map(|s| s.as_str()).collect();
        writer.set_cmd(&cmd);
    }

    let rootfs_blob = export_ostree_ref_to_blobdir(repo, commit, ocidir, compression)?;
    writer.set_root_layer(rootfs_blob);
    writer.complete()?;

    Ok(ImageReference {
        transport: Transport::OciDir,
        name: ocidir_path.to_str().unwrap().to_string(),
    })
}

/// Helper for `build()` that avoids generics
#[instrument(skip(repo))]
async fn build_impl(
    repo: &ostree::Repo,
    ostree_ref: &str,
    config: &Config,
    dest: &ImageReference,
) -> Result<String> {
    let compression = if dest.transport == Transport::ContainerStorage {
        Some(flate2::Compression::none())
    } else {
        None
    };
    if dest.transport == Transport::OciDir {
        let _copied: ImageReference = build_oci(
            repo,
            ostree_ref,
            Path::new(dest.name.as_str()),
            config,
            compression,
        )?;
    } else {
        let tempdir = tempfile::tempdir_in("/var/tmp")?;
        let tempdest = tempdir.path().join("d");
        let tempdest = tempdest.to_str().unwrap();
        let src = build_oci(repo, ostree_ref, Path::new(tempdest), config, compression)?;

        let mut cmd = skopeo::new_cmd();
        tracing::event!(Level::DEBUG, "Copying {} to {}", src, dest);
        cmd.stdout(std::process::Stdio::null())
            .arg("copy")
            .arg(src.to_string())
            .arg(dest.to_string());
        let proc = super::skopeo::spawn(cmd)?;
        let output = proc.wait_with_output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("skopeo failed: {}\n", stderr));
        }
    }
    let imgref = OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: dest.to_owned(),
    };
    // FIXME - it's obviously broken to do this push -> inspect cycle because of the possibility
    // of a race condition, but we need to patch skopeo to have the equivalent of `podman push --digestfile`.
    let info = super::import::fetch_manifest_info(&imgref).await?;
    Ok(info.manifest_digest)
}

/// Given an OSTree repository and ref, generate a container image.
///
/// The returned `ImageReference` will contain a digested (e.g. `@sha256:`) version of the destination.
pub async fn export<S: AsRef<str>>(
    repo: &ostree::Repo,
    ostree_ref: S,
    config: &Config,
    dest: &ImageReference,
) -> Result<String> {
    build_impl(repo, ostree_ref.as_ref(), config, dest).await
}
