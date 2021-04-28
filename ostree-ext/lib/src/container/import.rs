//! APIs for extracting OSTree commits from container images

use super::*;
use anyhow::anyhow;
use fn_error_context::context;
use futures::prelude::*;
use std::process::Stdio;
use tokio::io::AsyncRead;
use tracing::{event, instrument, Level};

/// Download the manifest for a target image.
#[context("Fetching manifest")]
pub async fn fetch_manifest_info(imgref: &ImageReference) -> Result<OstreeContainerManifestInfo> {
    let (_, manifest_digest) = fetch_manifest(imgref).await?;
    // Sadly this seems to be lost when pushing to e.g. quay.io, which means we can't use it.
    //    let commit = manifest
    //        .annotations
    //        .as_ref()
    //        .map(|a| a.get(OSTREE_COMMIT_LABEL))
    //        .flatten()
    //        .ok_or_else(|| anyhow!("Missing annotation {}", OSTREE_COMMIT_LABEL))?;
    Ok(OstreeContainerManifestInfo { manifest_digest })
}

/// Download the manifest for a target image.
#[context("Fetching manifest")]
async fn fetch_manifest(imgref: &ImageReference) -> Result<(oci::Manifest, String)> {
    let mut proc = skopeo::new_cmd();
    proc.args(&["inspect", "--raw"]).arg(imgref.to_string());
    proc.stdout(Stdio::piped());
    let proc = skopeo::spawn(proc)?.wait_with_output().await?;
    if !proc.status.success() {
        let errbuf = String::from_utf8_lossy(&proc.stderr);
        return Err(anyhow!("skopeo inspect failed\n{}", errbuf));
    }
    let raw_manifest = proc.stdout;
    let digest = openssl::hash::hash(openssl::hash::MessageDigest::sha256(), &raw_manifest)?;
    let digest = format!("sha256:{}", hex::encode(digest.as_ref()));
    Ok((serde_json::from_slice(&raw_manifest)?, digest))
}

/// Fetch a remote docker/OCI image into a local tarball, extract a specific blob.
async fn fetch_oci_archive_blob<'s>(
    imgref: &ImageReference,
    blobid: &str,
) -> Result<impl AsyncRead> {
    let mut proc = skopeo::new_cmd();
    proc.stdout(Stdio::null());
    let tempdir = tempfile::tempdir_in("/var/tmp")?;
    let target = &tempdir.path().join("d");
    tracing::trace!("skopeo pull starting to {:?}", target);
    proc.arg("copy")
        .arg(imgref.to_string())
        .arg(format!("oci://{}", target.to_str().unwrap()));
    skopeo::spawn(proc)?
        .wait()
        .err_into()
        .and_then(|e| async move {
            if !e.success() {
                return Err(anyhow!("skopeo failed: {}", e));
            }
            Ok(())
        })
        .await?;
    tracing::trace!("skopeo pull done");
    Ok(tokio::fs::File::open(target.join("blobs/sha256/").join(blobid)).await?)
}

/// The result of an import operation
#[derive(Debug)]
pub struct Import {
    /// The ostree commit that was imported
    pub ostree_commit: String,
    /// The image digest retrieved
    pub image_digest: String,
}

fn find_layer_blobid(manifest: &oci::Manifest) -> Result<String> {
    let layers: Vec<_> = manifest
        .layers
        .iter()
        .filter(|&layer| {
            matches!(
                layer.media_type.as_str(),
                super::oci::DOCKER_TYPE_LAYER | oci::OCI_TYPE_LAYER
            )
        })
        .collect();

    let n = layers.len();
    if let Some(layer) = layers.into_iter().next() {
        if n > 1 {
            Err(anyhow!("Expected 1 layer, found {}", n))
        } else {
            let digest = layer.digest.as_str();
            let hash = digest
                .strip_prefix("sha256:")
                .ok_or_else(|| anyhow!("Expected sha256: in digest: {}", digest))?;
            Ok(hash.into())
        }
    } else {
        Err(anyhow!("No layers found (orig: {})", manifest.layers.len()))
    }
}

/// Fetch a container image and import its embedded OSTree commit.
#[context("Importing {}", imgref)]
#[instrument(skip(repo))]
pub async fn import(repo: &ostree::Repo, imgref: &ImageReference) -> Result<Import> {
    let (manifest, image_digest) = fetch_manifest(imgref).await?;
    let manifest = &manifest;
    let layerid = find_layer_blobid(manifest)?;
    event!(Level::DEBUG, "target blob: {}", layerid);
    let blob = fetch_oci_archive_blob(imgref, layerid.as_str()).await?;
    let blob = tokio::io::BufReader::new(blob);
    // TODO also detect zstd
    let blob = async_compression::tokio::bufread::GzipDecoder::new(blob);
    let ostree_commit = crate::tar::import_tar(&repo, blob).await?;
    tracing::trace!("created commit {}", ostree_commit);
    Ok(Import {
        ostree_commit,
        image_digest,
    })
}
