//! APIs for extracting OSTree commits from container images

use super::*;
use anyhow::anyhow;
use fn_error_context::context;
use futures::prelude::*;
use std::io::prelude::*;
use std::process::Stdio;
use tokio::io::AsyncRead;

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

/// Bridge from AsyncRead to Read.
///
/// This creates a pipe and a "driver" future (which could be spawned or not).
fn copy_async_read_to_sync_pipe<S: AsyncRead + Unpin + Send + 'static>(
    s: S,
) -> Result<(impl Read, impl Future<Output = Result<()>>)> {
    let (pipein, mut pipeout) = os_pipe::pipe()?;

    let copier = async move {
        let mut input = tokio_util::io::ReaderStream::new(s).boxed();
        while let Some(buf) = input.next().await {
            let buf = buf?;
            // TODO blocking executor
            pipeout.write_all(&buf)?;
        }
        Ok::<_, anyhow::Error>(())
    };

    Ok((pipein, copier))
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
pub async fn import(repo: &ostree::Repo, imgref: &ImageReference) -> Result<Import> {
    let (manifest, image_digest) = fetch_manifest(imgref).await?;
    let manifest = &manifest;
    let layerid = find_layer_blobid(manifest)?;
    tracing::trace!("target blob: {}", layerid);
    let blob = fetch_oci_archive_blob(imgref, layerid.as_str()).await?;
    tracing::trace!("reading blob");
    let (pipein, copydriver) = copy_async_read_to_sync_pipe(blob)?;
    let repo = repo.clone();
    let import = tokio::task::spawn_blocking(move || {
        // FIXME don't hardcode compression, we need to detect it
        let gz = flate2::read::GzDecoder::new(pipein);
        crate::tar::import_tar(&repo, gz)
    })
    .map_err(anyhow::Error::msg);
    let (import, _copydriver) = tokio::try_join!(import, copydriver)?;
    let ostree_commit = import?;
    tracing::trace!("created commit {}", ostree_commit);
    Ok(Import {
        ostree_commit,
        image_digest,
    })
}
