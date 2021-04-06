//! APIs for creating container images from OSTree commits

use super::oci;
use super::Result;
use crate::tar as ostree_tar;
use anyhow::Context;
use fn_error_context::context;
use std::path::Path;

/// The location to store the generated image
pub enum Target<'a> {
    /// Generate an Open Containers image directory layout
    OciDir(&'a Path),
}

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
fn build_oci(repo: &ostree::Repo, commit: &str, ocidir: &Path) -> Result<()> {
    // Explicitly error if the target exists
    std::fs::create_dir(ocidir).context("Creating OCI dir")?;
    let ocidir = &openat::Dir::open(ocidir)?;
    let writer = &mut oci::OciWriter::new(ocidir)?;

    let rootfs_blob = export_ostree_ref_to_blobdir(repo, commit, ocidir)?;
    writer.set_root_layer(rootfs_blob);
    writer.complete()?;

    Ok(())
}

/// Helper for `build()` that avoids generics
fn build_impl(repo: &ostree::Repo, ostree_ref: &str, target: Target) -> Result<()> {
    match target {
        Target::OciDir(d) => build_oci(repo, ostree_ref, d),
    }
}

/// Given an OSTree repository and ref, generate a container image
pub fn build<S: AsRef<str>>(repo: &ostree::Repo, ostree_ref: S, target: Target) -> Result<()> {
    build_impl(repo, ostree_ref.as_ref(), target)
}
