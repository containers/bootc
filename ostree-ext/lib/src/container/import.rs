//! APIs for extracting OSTree commits from container images
//!
//! # External depenendency on container-image-proxy
//!
//! This code requires <https://github.com/cgwalters/container-image-proxy>
//! installed as a binary in $PATH.
//!
//! The rationale for this is that while there exist Rust crates to speak
//! the Docker distribution API, the Go library <https://github.com/containers/image/>
//! supports key things we want for production use like:
//!
//! - Image mirroring and remapping; effectively `man containers-registries.conf`
//!   For example, we need to support an administrator mirroring an ostree-container
//!   into a disconnected registry, without changing all the pull specs.
//! - Signing
//!
//! Additionally, the proxy "upconverts" manifests into OCI, so we don't need to care
//! about parsing the Docker manifest format (as used by most registries still).
//!
//!

// # Implementation
//
// First, we support explicitly fetching just the manifest: https://github.com/opencontainers/image-spec/blob/main/manifest.md
// This will give us information about the layers it contains, and crucially the digest (sha256) of
// the manifest is how higher level software can detect changes.
//
// Once we have the manifest, we expect it to point to a single `application/vnd.oci.image.layer.v1.tar+gzip` layer,
// which is exactly what is exported by the [`crate::tar::export`] process.

use super::*;
use anyhow::{anyhow, Context};
use fn_error_context::context;
use tokio::io::AsyncRead;
use tracing::{event, instrument, Level};

/// The result of an import operation
#[derive(Copy, Clone, Debug, Default)]
pub struct ImportProgress {
    /// Number of bytes downloaded (approximate)
    pub processed_bytes: u64,
}

type Progress = tokio::sync::watch::Sender<ImportProgress>;

/// A read wrapper that updates the download progress.
#[pin_project::pin_project]
struct ProgressReader<T> {
    #[pin]
    reader: T,
    #[pin]
    progress: Option<Progress>,
}

impl<T: AsyncRead> AsyncRead for ProgressReader<T> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.project();
        let len = buf.filled().len();
        match this.reader.poll_read(cx, buf) {
            v @ std::task::Poll::Ready(Ok(_)) => {
                if let Some(progress) = this.progress.as_ref().get_ref() {
                    let state = {
                        let mut state = *progress.borrow();
                        let newlen = buf.filled().len();
                        debug_assert!(newlen >= len);
                        let read = (newlen - len) as u64;
                        state.processed_bytes += read;
                        state
                    };
                    // Ignore errors, if the caller disconnected from progress that's OK.
                    let _ = progress.send(state);
                }
                v
            }
            o => o,
        }
    }
}

/// Download the manifest for a target image and its sha256 digest.
#[context("Fetching manifest")]
pub async fn fetch_manifest(imgref: &OstreeImageReference) -> Result<(Vec<u8>, String)> {
    let mut proxy = imageproxy::ImageProxy::new(&imgref.imgref).await?;
    let (digest, raw_manifest) = proxy.fetch_manifest().await?;
    Ok((raw_manifest, digest))
}

/// The result of an import operation
#[derive(Debug)]
pub struct Import {
    /// The ostree commit that was imported
    pub ostree_commit: String,
    /// The image digest retrieved
    pub image_digest: String,
}

fn require_one_layer_blob(manifest: &oci::Manifest) -> Result<&oci::ManifestLayer> {
    let n = manifest.layers.len();
    if let Some(layer) = manifest.layers.iter().next() {
        if n > 1 {
            Err(anyhow!("Expected 1 layer, found {}", n))
        } else {
            Ok(&layer)
        }
    } else {
        // Validated by find_layer_blobids()
        unreachable!()
    }
}

/// Configuration for container fetches.
#[derive(Debug, Default)]
pub struct ImportOptions {
    /// Channel which will receive progress updates
    pub progress: Option<tokio::sync::watch::Sender<ImportProgress>>,
}

/// Fetch a container image and import its embedded OSTree commit.
#[context("Importing {}", imgref)]
#[instrument(skip(repo, options))]
pub async fn import(
    repo: &ostree::Repo,
    imgref: &OstreeImageReference,
    options: Option<ImportOptions>,
) -> Result<Import> {
    let (manifest, image_digest) = fetch_manifest(imgref).await?;
    let ostree_commit = import_from_manifest(repo, imgref, &manifest, options).await?;
    Ok(Import {
        ostree_commit,
        image_digest,
    })
}

/// Fetch a container image using an in-memory manifest and import its embedded OSTree commit.
#[context("Importing {}", imgref)]
#[instrument(skip(repo, options, manifest_bytes))]
pub async fn import_from_manifest(
    repo: &ostree::Repo,
    imgref: &OstreeImageReference,
    manifest_bytes: &[u8],
    options: Option<ImportOptions>,
) -> Result<String> {
    if matches!(imgref.sigverify, SignatureSource::ContainerPolicy)
        && skopeo::container_policy_is_default_insecure()?
    {
        return Err(anyhow!("containers-policy.json specifies a default of `insecureAcceptAnything`; refusing usage"));
    }
    let options = options.unwrap_or_default();
    let manifest: oci::Manifest = serde_json::from_slice(manifest_bytes)?;
    let layer = require_one_layer_blob(&manifest)?;
    event!(Level::DEBUG, "target blob: {}", layer.digest.as_str());
    let mut proxy = imageproxy::ImageProxy::new(&imgref.imgref).await?;
    let blob = proxy.fetch_layer_decompress(layer).await?;
    let blob = ProgressReader {
        reader: blob,
        progress: options.progress,
    };
    let mut taropts: crate::tar::TarImportOptions = Default::default();
    match &imgref.sigverify {
        SignatureSource::OstreeRemote(remote) => taropts.remote = Some(remote.clone()),
        SignatureSource::ContainerPolicy | SignatureSource::ContainerPolicyAllowInsecure => {}
    }
    let ostree_commit = crate::tar::import_tar(repo, blob, Some(taropts))
        .await
        .with_context(|| format!("Parsing blob {}", layer.digest))?;
    // FIXME write ostree commit after proxy finalization
    proxy.finalize().await?;
    event!(Level::DEBUG, "created commit {}", ostree_commit);
    Ok(ostree_commit)
}
