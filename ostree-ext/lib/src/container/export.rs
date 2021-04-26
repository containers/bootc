//! APIs for creating container images from OSTree commits

use super::*;
use crate::{tar as ostree_tar, variant_utils};
use anyhow::Context;
use fn_error_context::context;
use std::path::Path;

/// Write an ostree commit to an OCI blob
#[context("Writing ostree root to blob")]
fn export_ostree_ref_to_blobdir(
    repo: &ostree::Repo,
    rev: &str,
    ocidir: &openat::Dir,
) -> Result<oci::Layer> {
    let commit = repo.resolve_rev(rev, false)?.unwrap();
    let mut w = oci::LayerWriter::new(ocidir)?;
    ostree_tar::export_commit(repo, commit.as_str(), &mut w)?;
    w.complete()
}

/// Generate an OCI image from a given ostree root
#[context("Building oci")]
fn build_oci(repo: &ostree::Repo, rev: &str, ocidir_path: &Path) -> Result<ImageReference> {
    // Explicitly error if the target exists
    std::fs::create_dir(ocidir_path).context("Creating OCI dir")?;
    let ocidir = &openat::Dir::open(ocidir_path)?;
    let writer = &mut oci::OciWriter::new(ocidir)?;

    let commit = repo.resolve_rev(rev, false)?.unwrap();
    let commit = commit.as_str();
    let (commit_v, _) = repo.load_commit(commit)?;
    let commit_meta = &variant_utils::variant_tuple_get(&commit_v, 0).unwrap();
    let commit_meta = glib::VariantDict::new(Some(commit_meta));

    if let Some(version) =
        commit_meta.lookup_value("version", Some(glib::VariantTy::new("s").unwrap()))
    {
        let version = version.get_str().unwrap();
        writer.add_config_annotation("version", version);
        writer.add_manifest_annotation("ostree.version", version);
    }

    writer.add_config_annotation(OSTREE_COMMIT_LABEL, commit);
    writer.add_manifest_annotation(OSTREE_COMMIT_LABEL, commit);

    let rootfs_blob = export_ostree_ref_to_blobdir(repo, commit, ocidir)?;
    writer.set_root_layer(rootfs_blob);
    writer.complete()?;

    Ok(ImageReference {
        transport: Transport::OciDir,
        name: ocidir_path.to_str().unwrap().to_string(),
    })
}

/// Helper for `build()` that avoids generics
async fn build_impl(
    repo: &ostree::Repo,
    ostree_ref: &str,
    dest: &ImageReference,
) -> Result<ImageReference> {
    if dest.transport == Transport::OciDir {
        let _copied: ImageReference = build_oci(repo, ostree_ref, Path::new(dest.name.as_str()))?;
    } else {
        let tempdir = tempfile::tempdir_in("/var/tmp")?;
        let tempdest = tempdir.path().join("d");
        let tempdest = tempdest.to_str().unwrap();
        let src = build_oci(repo, ostree_ref, Path::new(tempdest))?;

        let mut cmd = skopeo::new_cmd();
        log::trace!("Copying {} to {}", src, dest);
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
    // FIXME - it's obviously broken to do this push -> inspect cycle because of the possibility
    // of a race condition, but we need to patch skopeo to have the equivalent of `podman push --digestfile`.
    let info = super::import::fetch_manifest_info(dest).await?;
    Ok(dest.with_digest(info.manifest_digest.as_str()))
}

/// Given an OSTree repository and ref, generate a container image.
///
/// The returned `ImageReference` will contain a digested (e.g. `@sha256:`) version of the destination.
pub async fn export<S: AsRef<str>>(
    repo: &ostree::Repo,
    ostree_ref: S,
    dest: &ImageReference,
) -> Result<ImageReference> {
    build_impl(repo, ostree_ref.as_ref(), dest).await
}
