//! APIs for "unencapsulating" OSTree commits from container images
//!
//! This code only operates on container images that were created via
//! [`encapsulate`].
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
//! [`encapsulate`]: [`super::encapsulate()`]

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
use containers_image_proxy::{ImageProxy, OpenedImage};
use fn_error_context::context;
use futures_util::Future;
use oci_spec::image as oci_image;
use tokio::io::{AsyncBufRead, AsyncRead};
use tracing::{event, instrument, Level};

/// The result of an import operation
#[derive(Copy, Clone, Debug, Default)]
pub struct UnencapsulationProgress {
    /// Number of bytes downloaded (approximate)
    pub processed_bytes: u64,
}

type Progress = tokio::sync::watch::Sender<UnencapsulationProgress>;

/// A read wrapper that updates the download progress.
#[pin_project::pin_project]
#[derive(Debug)]
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

async fn fetch_manifest_impl(
    proxy: &mut ImageProxy,
    imgref: &OstreeImageReference,
) -> Result<(oci_spec::image::ImageManifest, String)> {
    let oi = &proxy.open_image(&imgref.imgref.to_string()).await?;
    let (digest, raw_manifest) = proxy.fetch_manifest(oi).await?;
    proxy.close_image(oi).await?;
    Ok((serde_json::from_slice(&raw_manifest)?, digest))
}

/// Download the manifest for a target image and its sha256 digest.
#[context("Fetching manifest")]
pub async fn fetch_manifest(
    imgref: &OstreeImageReference,
) -> Result<(oci_spec::image::ImageManifest, String)> {
    let mut proxy = ImageProxy::new().await?;
    fetch_manifest_impl(&mut proxy, imgref).await
}

/// The result of an import operation
#[derive(Debug)]
pub struct Import {
    /// The ostree commit that was imported
    pub ostree_commit: String,
    /// The image digest retrieved
    pub image_digest: String,
}

fn require_one_layer_blob(manifest: &oci_image::ImageManifest) -> Result<&oci_image::Descriptor> {
    let n = manifest.layers().len();
    if let Some(layer) = manifest.layers().get(0) {
        if n > 1 {
            Err(anyhow!("Expected 1 layer, found {}", n))
        } else {
            Ok(layer)
        }
    } else {
        // Validated by find_layer_blobids()
        unreachable!()
    }
}

/// Use this to process potential errors from a worker and a driver.
/// This is really a brutal hack around the fact that an error can occur
/// on either our side or in the proxy.  But if an error occurs on our
/// side, then we will close the pipe, which will *also* cause the proxy
/// to error out.
///
/// What we really want is for the proxy to tell us when it got an
/// error from us closing the pipe.  Or, we could store that state
/// on our side.  Both are slightly tricky, so we have this (again)
/// hacky thing where we just search for `broken pipe` in the error text.
///
/// Or to restate all of the above - what this function does is check
/// to see if the worker function had an error *and* if the proxy
/// had an error, but if the proxy's error ends in `broken pipe`
/// then it means the real only error is from the worker.
pub(crate) async fn join_fetch<T: std::fmt::Debug>(
    worker: impl Future<Output = Result<T>>,
    driver: impl Future<Output = Result<()>>,
) -> Result<T> {
    let (worker, driver) = tokio::join!(worker, driver);
    match (worker, driver) {
        (Ok(t), Ok(())) => Ok(t),
        (Err(worker), Err(driver)) => {
            let text = driver.root_cause().to_string();
            if text.ends_with("broken pipe") {
                Err(worker)
            } else {
                Err(worker.context(format!("proxy failure: {} and client error", text)))
            }
        }
        (Ok(_), Err(driver)) => Err(driver),
        (Err(worker), Ok(())) => Err(worker),
    }
}

/// Configuration for container fetches.
#[derive(Debug, Default)]
pub struct UnencapsulateOptions {
    /// Channel which will receive progress updates
    pub progress: Option<tokio::sync::watch::Sender<UnencapsulationProgress>>,
}

/// Fetch a container image and import its embedded OSTree commit.
#[context("Importing {}", imgref)]
#[instrument(skip(repo, options))]
pub async fn unencapsulate(
    repo: &ostree::Repo,
    imgref: &OstreeImageReference,
    options: Option<UnencapsulateOptions>,
) -> Result<Import> {
    let mut proxy = ImageProxy::new().await?;
    let (manifest, image_digest) = fetch_manifest_impl(&mut proxy, imgref).await?;
    let ostree_commit =
        unencapsulate_from_manifest_impl(repo, &mut proxy, imgref, &manifest, options).await?;
    Ok(Import {
        ostree_commit,
        image_digest,
    })
}

/// Create a decompressor for this MIME type, given a stream of input.
fn new_async_decompressor<'a>(
    media_type: &oci_image::MediaType,
    src: impl AsyncBufRead + Send + Unpin + 'a,
) -> Result<Box<dyn AsyncBufRead + Send + Unpin + 'a>> {
    match media_type {
        oci_image::MediaType::ImageLayerGzip => Ok(Box::new(tokio::io::BufReader::new(
            async_compression::tokio::bufread::GzipDecoder::new(src),
        ))),
        oci_image::MediaType::ImageLayer => Ok(Box::new(src)),
        o => Err(anyhow::anyhow!("Unhandled layer type: {}", o)),
    }
}

/// A wrapper for [`get_blob`] which fetches a layer and decompresses it.
#[instrument(skip(proxy, img, layer))]
pub(crate) async fn fetch_layer_decompress<'a>(
    proxy: &'a mut ImageProxy,
    img: &OpenedImage,
    layer: &oci_image::Descriptor,
) -> Result<(
    Box<dyn AsyncBufRead + Send + Unpin>,
    impl Future<Output = Result<()>> + 'a,
)> {
    tracing::debug!("fetching {}", layer.digest());
    let (blob, driver) = proxy
        .get_blob(img, layer.digest().as_str(), layer.size() as u64)
        .await?;
    let blob = new_async_decompressor(layer.media_type(), blob)?;
    Ok((blob, driver))
}

async fn unencapsulate_from_manifest_impl(
    repo: &ostree::Repo,
    proxy: &mut ImageProxy,
    imgref: &OstreeImageReference,
    manifest: &oci_spec::image::ImageManifest,
    options: Option<UnencapsulateOptions>,
) -> Result<String> {
    if matches!(imgref.sigverify, SignatureSource::ContainerPolicy)
        && skopeo::container_policy_is_default_insecure()?
    {
        return Err(anyhow!("containers-policy.json specifies a default of `insecureAcceptAnything`; refusing usage"));
    }
    let options = options.unwrap_or_default();
    let layer = require_one_layer_blob(manifest)?;
    event!(
        Level::DEBUG,
        "target blob digest:{} size: {}",
        layer.digest().as_str(),
        layer.size()
    );
    let oi = proxy.open_image(&imgref.imgref.to_string()).await?;
    let (blob, driver) = fetch_layer_decompress(proxy, &oi, layer).await?;
    let blob = ProgressReader {
        reader: blob,
        progress: options.progress,
    };
    let mut taropts: crate::tar::TarImportOptions = Default::default();
    match &imgref.sigverify {
        SignatureSource::OstreeRemote(remote) => taropts.remote = Some(remote.clone()),
        SignatureSource::ContainerPolicy | SignatureSource::ContainerPolicyAllowInsecure => {}
    }
    let import = crate::tar::import_tar(repo, blob, Some(taropts));
    let ostree_commit = join_fetch(import, driver)
        .await
        .with_context(|| format!("Parsing blob {}", layer.digest()))?;

    event!(Level::DEBUG, "created commit {}", ostree_commit);
    Ok(ostree_commit)
}

/// Fetch a container image using an in-memory manifest and import its embedded OSTree commit.
#[context("Importing {}", imgref)]
#[instrument(skip(repo, options, manifest))]
pub async fn unencapsulate_from_manifest(
    repo: &ostree::Repo,
    imgref: &OstreeImageReference,
    manifest: &oci_spec::image::ImageManifest,
    options: Option<UnencapsulateOptions>,
) -> Result<String> {
    let mut proxy = ImageProxy::new().await?;
    let r = unencapsulate_from_manifest_impl(repo, &mut proxy, imgref, manifest, options).await?;
    // FIXME write ostree commit after proxy finalization
    proxy.finalize().await?;
    Ok(r)
}
