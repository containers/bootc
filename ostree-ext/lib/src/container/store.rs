//! APIs for storing (layered) container images as OSTree commits
//!
//! # Extension of encapsulation support
//!
//! This code supports ingesting arbitrary layered container images from an ostree-exported
//! base.  See [`encapsulate`][`super::encapsulate()`] for more information on encaspulation of images.

use super::*;
use crate::logging::system_repo_journal_print;
use crate::refescape;
use crate::sysroot::SysrootLock;
use crate::utils::ResultExt;
use anyhow::{anyhow, Context};
use camino::{Utf8Path, Utf8PathBuf};
use cap_std_ext::cap_std::fs::Dir;
use containers_image_proxy::{ImageProxy, OpenedImage};
use fn_error_context::context;
use futures_util::TryFutureExt;
use oci_spec::image::{self as oci_image, Descriptor, History, ImageConfiguration, ImageManifest};
use ostree::prelude::{Cast, FileEnumeratorExt, FileExt, ToVariant};
use ostree::{gio, glib};
use rustix::fs::MetadataExt;
use std::collections::{BTreeSet, HashMap};
use std::iter::FromIterator;
use tokio::sync::mpsc::{Receiver, Sender};

/// Configuration for the proxy.
///
/// We re-export this rather than inventing our own wrapper
/// in the interest of avoiding duplication.
pub use containers_image_proxy::ImageProxyConfig;

/// The ostree ref prefix for blobs.
const LAYER_PREFIX: &str = "ostree/container/blob";
/// The ostree ref prefix for image references.
const IMAGE_PREFIX: &str = "ostree/container/image";
/// The ostree ref prefix for "base" image references that are used by derived images.
/// If you maintain tooling which is locally building derived commits, write a ref
/// with this prefix that is owned by your code.  It's a best practice to prefix the
/// ref with the project name, so the final ref may be of the form e.g. `ostree/container/baseimage/bootc/foo`.
pub const BASE_IMAGE_PREFIX: &str = "ostree/container/baseimage";

/// The key injected into the merge commit for the manifest digest.
pub(crate) const META_MANIFEST_DIGEST: &str = "ostree.manifest-digest";
/// The key injected into the merge commit with the manifest serialized as JSON.
const META_MANIFEST: &str = "ostree.manifest";
/// The key injected into the merge commit with the image configuration serialized as JSON.
const META_CONFIG: &str = "ostree.container.image-config";
/// Value of type `a{sa{su}}` containing number of filtered out files
pub const META_FILTERED: &str = "ostree.tar-filtered";
/// The type used to store content filtering information with `META_FILTERED`.
pub type MetaFilteredData = HashMap<String, HashMap<String, u32>>;

/// The ref prefixes which point to ostree deployments.  (TODO: Add an official API for this)
const OSTREE_BASE_DEPLOYMENT_REFS: &[&str] = &["ostree/0", "ostree/1"];
/// A layering violation we'll carry for a bit to band-aid over https://github.com/coreos/rpm-ostree/issues/4185
const RPMOSTREE_BASE_REFS: &[&str] = &["rpmostree/base"];

/// Convert e.g. sha256:12345... into `/ostree/container/blob/sha256_2B12345...`.
fn ref_for_blob_digest(d: &str) -> Result<String> {
    refescape::prefix_escape_for_ref(LAYER_PREFIX, d)
}

/// Convert e.g. sha256:12345... into `/ostree/container/blob/sha256_2B12345...`.
fn ref_for_layer(l: &oci_image::Descriptor) -> Result<String> {
    ref_for_blob_digest(l.digest().as_str())
}

/// Convert e.g. sha256:12345... into `/ostree/container/blob/sha256_2B12345...`.
fn ref_for_image(l: &ImageReference) -> Result<String> {
    refescape::prefix_escape_for_ref(IMAGE_PREFIX, &l.to_string())
}

/// Sent across a channel to track start and end of a container fetch.
#[derive(Debug)]
pub enum ImportProgress {
    /// Started fetching this layer.
    OstreeChunkStarted(Descriptor),
    /// Successfully completed the fetch of this layer.
    OstreeChunkCompleted(Descriptor),
    /// Started fetching this layer.
    DerivedLayerStarted(Descriptor),
    /// Successfully completed the fetch of this layer.
    DerivedLayerCompleted(Descriptor),
}

impl ImportProgress {
    /// Returns `true` if this message signifies the start of a new layer being fetched.
    pub fn is_starting(&self) -> bool {
        match self {
            ImportProgress::OstreeChunkStarted(_) => true,
            ImportProgress::OstreeChunkCompleted(_) => false,
            ImportProgress::DerivedLayerStarted(_) => true,
            ImportProgress::DerivedLayerCompleted(_) => false,
        }
    }
}

/// Sent across a channel to track the byte-level progress of a layer fetch.
#[derive(Debug)]
pub struct LayerProgress {
    /// Index of the layer in the manifest
    pub layer_index: usize,
    /// Number of bytes downloaded
    pub fetched: u64,
    /// Total number of bytes outstanding
    pub total: u64,
}

/// State of an already pulled layered image.
#[derive(Debug, PartialEq, Eq)]
pub struct LayeredImageState {
    /// The base ostree commit
    pub base_commit: String,
    /// The merge commit unions all layers
    pub merge_commit: String,
    /// Whether or not the image has multiple layers.
    pub is_layered: bool,
    /// The digest of the original manifest
    pub manifest_digest: String,
    /// The image manfiest
    pub manifest: ImageManifest,
    /// The image configuration; for v0 images, may not be available.
    pub configuration: Option<ImageConfiguration>,
    /// Metadata for (cached, previously fetched) updates to the image, if any.
    pub cached_update: Option<CachedImageUpdate>,
}

impl LayeredImageState {
    /// Return the default ostree commit digest for this image.
    ///
    /// If this is a non-layered image, the merge commit will be
    /// ignored, and the base commit returned.
    ///
    /// Otherwise, this returns the merge commit.
    pub fn get_commit(&self) -> &str {
        if self.is_layered {
            self.merge_commit.as_str()
        } else {
            self.base_commit.as_str()
        }
    }

    /// Retrieve the container image version.
    pub fn version(&self) -> Option<&str> {
        self.configuration
            .as_ref()
            .and_then(super::version_for_config)
    }
}

/// Locally cached metadata for an update to an existing image.
#[derive(Debug, PartialEq, Eq)]
pub struct CachedImageUpdate {
    /// The image manifest
    pub manifest: ImageManifest,
    /// The image configuration
    pub config: ImageConfiguration,
    /// The digest of the manifest
    pub manifest_digest: String,
}

impl CachedImageUpdate {
    /// Retrieve the container image version.
    pub fn version(&self) -> Option<&str> {
        super::version_for_config(&self.config)
    }
}

/// Context for importing a container image.
#[derive(Debug)]
pub struct ImageImporter {
    repo: ostree::Repo,
    pub(crate) proxy: ImageProxy,
    imgref: OstreeImageReference,
    target_imgref: Option<OstreeImageReference>,
    no_imgref: bool,  // If true, do not write final image ref
    disable_gc: bool, // If true, don't prune unused image layers
    /// If true, require the image has the bootable flag
    require_bootable: bool,
    pub(crate) proxy_img: OpenedImage,

    layer_progress: Option<Sender<ImportProgress>>,
    layer_byte_progress: Option<tokio::sync::watch::Sender<Option<LayerProgress>>>,
}

/// Result of invoking [`ImageImporter::prepare`].
#[derive(Debug)]
pub enum PrepareResult {
    /// The image reference is already present; the contained string is the OSTree commit.
    AlreadyPresent(Box<LayeredImageState>),
    /// The image needs to be downloaded
    Ready(Box<PreparedImport>),
}

/// A container image layer with associated downloaded-or-not state.
#[derive(Debug)]
pub struct ManifestLayerState {
    pub(crate) layer: oci_image::Descriptor,
    /// The ostree ref name for this layer.
    pub ostree_ref: String,
    /// The ostree commit that caches this layer, if present.
    pub commit: Option<String>,
}

impl ManifestLayerState {
    /// The cryptographic checksum.
    pub fn digest(&self) -> &str {
        self.layer.digest().as_str()
    }

    /// The (possibly compressed) size.
    pub fn size(&self) -> u64 {
        self.layer.size() as u64
    }
}

/// Information about which layers need to be downloaded.
#[derive(Debug)]
pub struct PreparedImport {
    /// The manifest digest that was found
    pub manifest_digest: String,
    /// The deserialized manifest.
    pub manifest: oci_image::ImageManifest,
    /// The deserialized configuration.
    pub config: oci_image::ImageConfiguration,
    /// The previous manifest
    pub previous_state: Option<Box<LayeredImageState>>,
    /// The previously stored manifest digest.
    pub previous_manifest_digest: Option<String>,
    /// The previously stored image ID.
    pub previous_imageid: Option<String>,
    /// The layers containing split objects
    pub ostree_layers: Vec<ManifestLayerState>,
    /// The layer for the ostree commit.
    pub ostree_commit_layer: ManifestLayerState,
    /// Any further non-ostree (derived) layers.
    pub layers: Vec<ManifestLayerState>,
}

impl PreparedImport {
    /// Iterate over all layers; the commit layer, the ostree split object layers, and any non-ostree layers.
    pub fn all_layers(&self) -> impl Iterator<Item = &ManifestLayerState> {
        std::iter::once(&self.ostree_commit_layer)
            .chain(self.ostree_layers.iter())
            .chain(self.layers.iter())
    }

    /// Retrieve the container image version.
    pub fn version(&self) -> Option<&str> {
        super::version_for_config(&self.config)
    }

    /// If this image is using any deprecated features, return a message saying so.
    pub fn deprecated_warning(&self) -> Option<&'static str> {
        None
    }

    /// Iterate over all layers paired with their history entry.
    /// An error will be returned if the history does not cover all entries.
    pub fn layers_with_history(
        &self,
    ) -> impl Iterator<Item = Result<(&ManifestLayerState, &History)>> {
        // FIXME use .filter(|h| h.empty_layer.unwrap_or_default()) after https://github.com/containers/oci-spec-rs/pull/100 lands.
        let truncated = std::iter::once_with(|| Err(anyhow::anyhow!("Truncated history")));
        let history = self.config.history().iter().map(Ok).chain(truncated);
        self.all_layers()
            .zip(history)
            .map(|(s, h)| h.map(|h| (s, h)))
    }

    /// Iterate over all layers that are not present, along with their history description.
    pub fn layers_to_fetch(&self) -> impl Iterator<Item = Result<(&ManifestLayerState, &str)>> {
        self.layers_with_history().filter_map(|r| {
            r.map(|(l, h)| {
                l.commit.is_none().then(|| {
                    let comment = h.created_by().as_deref().unwrap_or("");
                    (l, comment)
                })
            })
            .transpose()
        })
    }

    /// Common helper to format a string for the status
    pub(crate) fn format_layer_status(&self) -> Option<String> {
        let (stored, to_fetch, to_fetch_size) =
            self.all_layers()
                .fold((0u32, 0u32, 0u64), |(stored, to_fetch, sz), v| {
                    if v.commit.is_some() {
                        (stored + 1, to_fetch, sz)
                    } else {
                        (stored, to_fetch + 1, sz + v.size())
                    }
                });
        (to_fetch > 0).then(|| {
            let size = crate::glib::format_size(to_fetch_size);
            format!("layers already present: {stored}; layers needed: {to_fetch} ({size})")
        })
    }
}

// Given a manifest, compute its ostree ref name and cached ostree commit
pub(crate) fn query_layer(
    repo: &ostree::Repo,
    layer: oci_image::Descriptor,
) -> Result<ManifestLayerState> {
    let ostree_ref = ref_for_layer(&layer)?;
    let commit = repo.resolve_rev(&ostree_ref, true)?.map(|s| s.to_string());
    Ok(ManifestLayerState {
        layer,
        ostree_ref,
        commit,
    })
}

#[context("Reading manifest data from commit")]
fn manifest_data_from_commitmeta(
    commit_meta: &glib::VariantDict,
) -> Result<(oci_image::ImageManifest, String)> {
    let digest = commit_meta
        .lookup(META_MANIFEST_DIGEST)?
        .ok_or_else(|| anyhow!("Missing {} metadata on merge commit", META_MANIFEST_DIGEST))?;
    let manifest_bytes: String = commit_meta
        .lookup::<String>(META_MANIFEST)?
        .ok_or_else(|| anyhow!("Failed to find {} metadata key", META_MANIFEST))?;
    let r = serde_json::from_str(&manifest_bytes)?;
    Ok((r, digest))
}

fn image_config_from_commitmeta(
    commit_meta: &glib::VariantDict,
) -> Result<Option<ImageConfiguration>> {
    commit_meta
        .lookup::<String>(META_CONFIG)?
        .filter(|v| v != "null") // Format v0 apparently old versions injected `null` here sadly...
        .map(|v| serde_json::from_str(&v).map_err(anyhow::Error::msg))
        .transpose()
}

/// Return the original digest of the manifest stored in the commit metadata.
/// This will be a string of the form e.g. `sha256:<digest>`.
///
/// This can be used to uniquely identify the image.  For example, it can be used
/// in a "digested pull spec" like `quay.io/someuser/exampleos@sha256:...`.
pub fn manifest_digest_from_commit(commit: &glib::Variant) -> Result<String> {
    let commit_meta = &commit.child_value(0);
    let commit_meta = &glib::VariantDict::new(Some(commit_meta));
    Ok(manifest_data_from_commitmeta(commit_meta)?.1)
}

/// Given a target diffid, return its corresponding layer.  In our current model,
/// we require a 1-to-1 mapping between the two up until the ostree level.
/// For a bit more information on this, see https://github.com/opencontainers/image-spec/blob/main/config.md
fn layer_from_diffid<'a>(
    layout: ExportLayout,
    manifest: &'a ImageManifest,
    config: &ImageConfiguration,
    diffid: &str,
) -> Result<&'a Descriptor> {
    let idx = config
        .rootfs()
        .diff_ids()
        .iter()
        .position(|x| x.as_str() == diffid)
        .ok_or_else(|| anyhow!("Missing {} {}", layout.label(), diffid))?;
    manifest.layers().get(idx).ok_or_else(|| {
        anyhow!(
            "diffid position {} exceeds layer count {}",
            idx,
            manifest.layers().len()
        )
    })
}

#[context("Parsing manifest layout")]
pub(crate) fn parse_manifest_layout<'a>(
    manifest: &'a ImageManifest,
    config: &ImageConfiguration,
) -> Result<(&'a Descriptor, Vec<&'a Descriptor>, Vec<&'a Descriptor>)> {
    let config_labels = super::labels_of(config);

    let first_layer = manifest
        .layers()
        .get(0)
        .ok_or_else(|| anyhow!("No layers in manifest"))?;
    let info = config_labels.and_then(|labels| {
        labels
            .get(ExportLayout::V1.label())
            .map(|v| (ExportLayout::V1, v))
            .or_else(|| {
                labels
                    .get(ExportLayout::V0.label())
                    .map(|v| (ExportLayout::V0, v))
            })
    });

    let (layout, target_diffid) = info.ok_or_else(|| {
        anyhow!(
            "No {} label found, not an ostree encapsulated container",
            ExportLayout::V1.label()
        )
    })?;
    let target_layer = layer_from_diffid(layout, manifest, config, target_diffid.as_str())?;
    let mut chunk_layers = Vec::new();
    let mut derived_layers = Vec::new();
    let mut after_target = false;
    // Gather the ostree layer
    let ostree_layer = match layout {
        ExportLayout::V0 => target_layer,
        ExportLayout::V1 => first_layer,
    };
    // Now, we need to handle the split differently in chunked v1 vs v0
    match layout {
        ExportLayout::V0 => {
            let label = layout.label();
            anyhow::bail!("This legacy format using the {label} label is no longer supported");
        }
        ExportLayout::V1 => {
            for layer in manifest.layers() {
                if layer == target_layer {
                    if after_target {
                        anyhow::bail!("Multiple entries for {}", layer.digest());
                    }
                    after_target = true;
                    if layer != ostree_layer {
                        chunk_layers.push(layer);
                    }
                } else if !after_target {
                    if layer != ostree_layer {
                        chunk_layers.push(layer);
                    }
                } else {
                    derived_layers.push(layer);
                }
            }
        }
    }

    Ok((ostree_layer, chunk_layers, derived_layers))
}

/// Find the timestamp of the manifest (or config), ignoring errors.
fn timestamp_of_manifest_or_config(
    manifest: &ImageManifest,
    config: &ImageConfiguration,
) -> Option<u64> {
    // The manifest timestamp seems to not be widely used, but let's
    // try it in preference to the config one.
    let timestamp = manifest
        .annotations()
        .as_ref()
        .and_then(|a| a.get(oci_image::ANNOTATION_CREATED))
        .or_else(|| config.created().as_ref());
    // Try to parse the timestamp
    timestamp
        .map(|t| {
            chrono::DateTime::parse_from_rfc3339(t)
                .context("Failed to parse manifest timestamp")
                .map(|t| t.timestamp() as u64)
        })
        .transpose()
        .log_err_default()
}

impl ImageImporter {
    /// The metadata key used in ostree commit metadata to serialize
    const CACHED_KEY_MANIFEST_DIGEST: &'static str = "ostree-ext.cached.manifest-digest";
    const CACHED_KEY_MANIFEST: &'static str = "ostree-ext.cached.manifest";
    const CACHED_KEY_CONFIG: &'static str = "ostree-ext.cached.config";

    /// Create a new importer.
    #[context("Creating importer")]
    pub async fn new(
        repo: &ostree::Repo,
        imgref: &OstreeImageReference,
        mut config: ImageProxyConfig,
    ) -> Result<Self> {
        if imgref.imgref.transport == Transport::ContainerStorage {
            // Fetching from containers-storage, may require privileges to read files
            merge_default_container_proxy_opts_with_isolation(&mut config, None)?;
        } else {
            // Apply our defaults to the proxy config
            merge_default_container_proxy_opts(&mut config)?;
        }
        let proxy = ImageProxy::new_with_config(config).await?;

        system_repo_journal_print(
            repo,
            libsystemd::logging::Priority::Info,
            &format!("Fetching {}", imgref),
        );

        let proxy_img = proxy.open_image(&imgref.imgref.to_string()).await?;
        let repo = repo.clone();
        Ok(ImageImporter {
            repo,
            proxy,
            proxy_img,
            target_imgref: None,
            no_imgref: false,
            disable_gc: false,
            require_bootable: false,
            imgref: imgref.clone(),
            layer_progress: None,
            layer_byte_progress: None,
        })
    }

    /// Write cached data as if the image came from this source.
    pub fn set_target(&mut self, target: &OstreeImageReference) {
        self.target_imgref = Some(target.clone())
    }

    /// Do not write the final image ref, but do write refs for shared layers.
    /// This is useful in scenarios where you want to "pre-pull" an image,
    /// but in such a way that it does not need to be manually removed later.
    pub fn set_no_imgref(&mut self) {
        self.no_imgref = true;
    }

    /// Require that the image has the bootable metadata field
    pub fn require_bootable(&mut self) {
        self.require_bootable = true;
    }

    /// Do not prune image layers.
    pub fn disable_gc(&mut self) {
        self.disable_gc = true;
    }

    /// Determine if there is a new manifest, and if so return its digest.
    /// This will also serialize the new manifest and configuration into
    /// metadata associated with the image, so that invocations of `[query_cached]`
    /// can re-fetch it without accessing the network.
    #[context("Preparing import")]
    pub async fn prepare(&mut self) -> Result<PrepareResult> {
        self.prepare_internal(false).await
    }

    /// Create a channel receiver that will get notifications for layer fetches.
    pub fn request_progress(&mut self) -> Receiver<ImportProgress> {
        assert!(self.layer_progress.is_none());
        let (s, r) = tokio::sync::mpsc::channel(2);
        self.layer_progress = Some(s);
        r
    }

    /// Create a channel receiver that will get notifications for byte-level progress of layer fetches.
    pub fn request_layer_progress(
        &mut self,
    ) -> tokio::sync::watch::Receiver<Option<LayerProgress>> {
        assert!(self.layer_byte_progress.is_none());
        let (s, r) = tokio::sync::watch::channel(None);
        self.layer_byte_progress = Some(s);
        r
    }

    /// Serialize the metadata about a pending fetch as detached metadata on the commit object,
    /// so it can be retrieved later offline
    #[context("Writing cached pending manifest")]
    pub(crate) async fn cache_pending(
        &self,
        commit: &str,
        manifest_digest: &str,
        manifest: &ImageManifest,
        config: &ImageConfiguration,
    ) -> Result<()> {
        let commitmeta = glib::VariantDict::new(None);
        commitmeta.insert(Self::CACHED_KEY_MANIFEST_DIGEST, manifest_digest);
        let cached_manifest = serde_json::to_string(manifest).context("Serializing manifest")?;
        commitmeta.insert(Self::CACHED_KEY_MANIFEST, cached_manifest);
        let cached_config = serde_json::to_string(config).context("Serializing config")?;
        commitmeta.insert(Self::CACHED_KEY_CONFIG, cached_config);
        let commitmeta = commitmeta.to_variant();
        // Clone these to move into blocking method
        let commit = commit.to_string();
        let repo = self.repo.clone();
        crate::tokio_util::spawn_blocking_cancellable_flatten(move |cancellable| {
            repo.write_commit_detached_metadata(&commit, Some(&commitmeta), Some(cancellable))
                .map_err(anyhow::Error::msg)
        })
        .await
    }

    /// Given existing metadata (manifest, config, previous image statE) generate a PreparedImport structure
    /// which e.g. includes a diff of the layers.
    fn create_prepared_import(
        &mut self,
        manifest_digest: String,
        manifest: ImageManifest,
        config: ImageConfiguration,
        previous_state: Option<Box<LayeredImageState>>,
        previous_imageid: Option<String>,
    ) -> Result<Box<PreparedImport>> {
        let config_labels = super::labels_of(&config);
        if self.require_bootable {
            let bootable_key = *ostree::METADATA_KEY_BOOTABLE;
            let bootable = config_labels.map_or(false, |l| l.contains_key(bootable_key));
            if !bootable {
                anyhow::bail!("Target image does not have {bootable_key} label");
            }
        }

        let (commit_layer, component_layers, remaining_layers) =
            parse_manifest_layout(&manifest, &config)?;

        let query = |l: &Descriptor| query_layer(&self.repo, l.clone());
        let commit_layer = query(commit_layer)?;
        let component_layers = component_layers
            .into_iter()
            .map(query)
            .collect::<Result<Vec<_>>>()?;
        let remaining_layers = remaining_layers
            .into_iter()
            .map(query)
            .collect::<Result<Vec<_>>>()?;

        let previous_manifest_digest = previous_state.as_ref().map(|s| s.manifest_digest.clone());
        let imp = PreparedImport {
            manifest_digest,
            manifest,
            config,
            previous_state,
            previous_manifest_digest,
            previous_imageid,
            ostree_layers: component_layers,
            ostree_commit_layer: commit_layer,
            layers: remaining_layers,
        };
        Ok(Box::new(imp))
    }

    /// Determine if there is a new manifest, and if so return its digest.
    #[context("Fetching manifest")]
    pub(crate) async fn prepare_internal(&mut self, verify_layers: bool) -> Result<PrepareResult> {
        match &self.imgref.sigverify {
            SignatureSource::ContainerPolicy if skopeo::container_policy_is_default_insecure()? => {
                return Err(anyhow!("containers-policy.json specifies a default of `insecureAcceptAnything`; refusing usage"));
            }
            SignatureSource::OstreeRemote(_) if verify_layers => {
                return Err(anyhow!(
                    "Cannot currently verify layered containers via ostree remote"
                ));
            }
            _ => {}
        }

        let (manifest_digest, manifest) = self.proxy.fetch_manifest(&self.proxy_img).await?;
        let new_imageid = manifest.config().digest().as_str();

        // Query for previous stored state

        let (previous_state, previous_imageid) =
            if let Some(previous_state) = try_query_image_ref(&self.repo, &self.imgref.imgref)? {
                // If the manifest digests match, we're done.
                if previous_state.manifest_digest == manifest_digest {
                    return Ok(PrepareResult::AlreadyPresent(previous_state));
                }
                // Failing that, if they have the same imageID, we're also done.
                let previous_imageid = previous_state.manifest.config().digest().as_str();
                if previous_imageid == new_imageid {
                    return Ok(PrepareResult::AlreadyPresent(previous_state));
                }
                let previous_imageid = previous_imageid.to_string();
                (Some(previous_state), Some(previous_imageid))
            } else {
                (None, None)
            };

        let config = self.proxy.fetch_config(&self.proxy_img).await?;

        // If there is a currently fetched image, cache the new pending manifest+config
        // as detached commit metadata, so that future fetches can query it offline.
        if let Some(previous_state) = previous_state.as_ref() {
            self.cache_pending(
                previous_state.merge_commit.as_str(),
                manifest_digest.as_str(),
                &manifest,
                &config,
            )
            .await?;
        }

        let imp = self.create_prepared_import(
            manifest_digest,
            manifest,
            config,
            previous_state,
            previous_imageid,
        )?;
        Ok(PrepareResult::Ready(imp))
    }

    /// Extract the base ostree commit.
    #[context("Unencapsulating base")]
    pub(crate) async fn unencapsulate_base(
        &mut self,
        import: &mut store::PreparedImport,
        write_refs: bool,
    ) -> Result<()> {
        tracing::debug!("Fetching base");
        if matches!(self.imgref.sigverify, SignatureSource::ContainerPolicy)
            && skopeo::container_policy_is_default_insecure()?
        {
            return Err(anyhow!("containers-policy.json specifies a default of `insecureAcceptAnything`; refusing usage"));
        }
        let remote = match &self.imgref.sigverify {
            SignatureSource::OstreeRemote(remote) => Some(remote.clone()),
            SignatureSource::ContainerPolicy | SignatureSource::ContainerPolicyAllowInsecure => {
                None
            }
        };
        let des_layers = self.proxy.get_layer_info(&self.proxy_img).await?;
        for layer in import.ostree_layers.iter_mut() {
            if layer.commit.is_some() {
                continue;
            }
            if let Some(p) = self.layer_progress.as_ref() {
                p.send(ImportProgress::OstreeChunkStarted(layer.layer.clone()))
                    .await?;
            }
            let (blob, driver) = fetch_layer_decompress(
                &mut self.proxy,
                &self.proxy_img,
                &import.manifest,
                &layer.layer,
                self.layer_byte_progress.as_ref(),
                des_layers.as_ref(),
                self.imgref.imgref.transport,
            )
            .await?;
            let repo = self.repo.clone();
            let target_ref = layer.ostree_ref.clone();
            let import_task =
                crate::tokio_util::spawn_blocking_cancellable_flatten(move |cancellable| {
                    let txn = repo.auto_transaction(Some(cancellable))?;
                    let mut importer = crate::tar::Importer::new_for_object_set(&repo);
                    let blob = tokio_util::io::SyncIoBridge::new(blob);
                    let mut archive = tar::Archive::new(blob);
                    importer.import_objects(&mut archive, Some(cancellable))?;
                    let commit = if write_refs {
                        let commit = importer.finish_import_object_set()?;
                        repo.transaction_set_ref(None, &target_ref, Some(commit.as_str()));
                        tracing::debug!("Wrote {} => {}", target_ref, commit);
                        Some(commit)
                    } else {
                        None
                    };
                    txn.commit(Some(cancellable))?;
                    Ok::<_, anyhow::Error>(commit)
                })
                .map_err(|e| e.context(format!("Layer {}", layer.digest())));
            let commit = super::unencapsulate::join_fetch(import_task, driver).await?;
            layer.commit = commit;
            if let Some(p) = self.layer_progress.as_ref() {
                p.send(ImportProgress::OstreeChunkCompleted(layer.layer.clone()))
                    .await?;
            }
        }
        if import.ostree_commit_layer.commit.is_none() {
            if let Some(p) = self.layer_progress.as_ref() {
                p.send(ImportProgress::OstreeChunkStarted(
                    import.ostree_commit_layer.layer.clone(),
                ))
                .await?;
            }
            let (blob, driver) = fetch_layer_decompress(
                &mut self.proxy,
                &self.proxy_img,
                &import.manifest,
                &import.ostree_commit_layer.layer,
                self.layer_byte_progress.as_ref(),
                des_layers.as_ref(),
                self.imgref.imgref.transport,
            )
            .await?;
            let repo = self.repo.clone();
            let target_ref = import.ostree_commit_layer.ostree_ref.clone();
            let import_task =
                crate::tokio_util::spawn_blocking_cancellable_flatten(move |cancellable| {
                    let txn = repo.auto_transaction(Some(cancellable))?;
                    let mut importer = crate::tar::Importer::new_for_commit(&repo, remote);
                    let blob = tokio_util::io::SyncIoBridge::new(blob);
                    let mut archive = tar::Archive::new(blob);
                    importer.import_commit(&mut archive, Some(cancellable))?;
                    let commit = importer.finish_import_commit();
                    if write_refs {
                        repo.transaction_set_ref(None, &target_ref, Some(commit.as_str()));
                        tracing::debug!("Wrote {} => {}", target_ref, commit);
                    }
                    repo.mark_commit_partial(&commit, false)?;
                    txn.commit(Some(cancellable))?;
                    Ok::<_, anyhow::Error>(commit)
                });
            let commit = super::unencapsulate::join_fetch(import_task, driver).await?;
            import.ostree_commit_layer.commit = Some(commit);
            if let Some(p) = self.layer_progress.as_ref() {
                p.send(ImportProgress::OstreeChunkCompleted(
                    import.ostree_commit_layer.layer.clone(),
                ))
                .await?;
            }
        };
        Ok(())
    }

    /// Retrieve an inner ostree commit.
    ///
    /// This does not write cached references for each blob, and errors out if
    /// the image has any non-ostree layers.
    pub async fn unencapsulate(mut self) -> Result<Import> {
        let mut prep = match self.prepare_internal(false).await? {
            PrepareResult::AlreadyPresent(_) => {
                panic!("Should not have image present for unencapsulation")
            }
            PrepareResult::Ready(r) => r,
        };
        if !prep.layers.is_empty() {
            anyhow::bail!("Image has {} non-ostree layers", prep.layers.len());
        }
        let deprecated_warning = prep.deprecated_warning().map(ToOwned::to_owned);
        self.unencapsulate_base(&mut prep, false).await?;
        // TODO change the imageproxy API to ensure this happens automatically when
        // the image reference is dropped
        self.proxy.close_image(&self.proxy_img).await?;
        let ostree_commit = prep.ostree_commit_layer.commit.unwrap();
        let image_digest = prep.manifest_digest;
        Ok(Import {
            ostree_commit,
            image_digest,
            deprecated_warning,
        })
    }

    /// Import a layered container image.
    ///
    /// If enabled, this will also prune unused container image layers.
    #[context("Importing")]
    pub async fn import(
        mut self,
        mut import: Box<PreparedImport>,
    ) -> Result<Box<LayeredImageState>> {
        if let Some(status) = import.format_layer_status() {
            system_repo_journal_print(&self.repo, libsystemd::logging::Priority::Info, &status);
        }
        // First download all layers for the base image (if necessary) - we need the SELinux policy
        // there to label all following layers.
        self.unencapsulate_base(&mut import, true).await?;
        let des_layers = self.proxy.get_layer_info(&self.proxy_img).await?;
        let mut proxy = self.proxy;
        let proxy_img = self.proxy_img;
        let target_imgref = self.target_imgref.as_ref().unwrap_or(&self.imgref);
        let base_commit = import.ostree_commit_layer.commit.clone().unwrap();

        let root_is_transient = {
            let rootf = self
                .repo
                .read_commit(&base_commit, gio::Cancellable::NONE)?
                .0;
            let rootf = rootf.downcast_ref::<ostree::RepoFile>().unwrap();
            crate::ostree_prepareroot::overlayfs_root_enabled(rootf)?
        };
        tracing::debug!("Base rootfs is transient: {root_is_transient}");

        let ostree_ref = ref_for_image(&target_imgref.imgref)?;

        let mut layer_commits = Vec::new();
        let mut layer_filtered_content: MetaFilteredData = HashMap::new();
        for layer in import.layers {
            if let Some(c) = layer.commit {
                tracing::debug!("Reusing fetched commit {}", c);
                layer_commits.push(c.to_string());
            } else {
                if let Some(p) = self.layer_progress.as_ref() {
                    p.send(ImportProgress::DerivedLayerStarted(layer.layer.clone()))
                        .await?;
                }
                let (blob, driver) = super::unencapsulate::fetch_layer_decompress(
                    &mut proxy,
                    &proxy_img,
                    &import.manifest,
                    &layer.layer,
                    self.layer_byte_progress.as_ref(),
                    des_layers.as_ref(),
                    self.imgref.imgref.transport,
                )
                .await?;
                // An important aspect of this is that we SELinux label the derived layers using
                // the base policy.
                let opts = crate::tar::WriteTarOptions {
                    base: Some(base_commit.clone()),
                    selinux: true,
                    allow_nonusr: root_is_transient,
                };
                let r =
                    crate::tar::write_tar(&self.repo, blob, layer.ostree_ref.as_str(), Some(opts));
                let r = super::unencapsulate::join_fetch(r, driver)
                    .await
                    .with_context(|| format!("Parsing layer blob {}", layer.digest()))?;
                layer_commits.push(r.commit);
                if !r.filtered.is_empty() {
                    let filtered = HashMap::from_iter(r.filtered.into_iter());
                    tracing::debug!("Found {} filtered toplevels", filtered.len());
                    layer_filtered_content.insert(layer.digest().to_string(), filtered);
                } else {
                    tracing::debug!("No filtered content");
                }
                if let Some(p) = self.layer_progress.as_ref() {
                    p.send(ImportProgress::DerivedLayerCompleted(layer.layer.clone()))
                        .await?;
                }
            }
        }

        // TODO change the imageproxy API to ensure this happens automatically when
        // the image reference is dropped
        proxy.close_image(&proxy_img).await?;

        // We're done with the proxy, make sure it didn't have any errors.
        proxy.finalize().await?;
        tracing::debug!("finalized proxy");

        let serialized_manifest = serde_json::to_string(&import.manifest)?;
        let serialized_config = serde_json::to_string(&import.config)?;
        let mut metadata = HashMap::new();
        metadata.insert(META_MANIFEST_DIGEST, import.manifest_digest.to_variant());
        metadata.insert(META_MANIFEST, serialized_manifest.to_variant());
        metadata.insert(META_CONFIG, serialized_config.to_variant());
        metadata.insert(
            "ostree.importer.version",
            env!("CARGO_PKG_VERSION").to_variant(),
        );
        let filtered = layer_filtered_content.to_variant();
        metadata.insert(META_FILTERED, filtered);
        let metadata = metadata.to_variant();

        let timestamp = timestamp_of_manifest_or_config(&import.manifest, &import.config)
            .unwrap_or_else(|| chrono::offset::Utc::now().timestamp() as u64);
        // Destructure to transfer ownership to thread
        let repo = self.repo;
        let state = crate::tokio_util::spawn_blocking_cancellable_flatten(
            move |cancellable| -> Result<Box<LayeredImageState>> {
                use rustix::fd::AsRawFd;

                let cancellable = Some(cancellable);
                let repo = &repo;
                let txn = repo.auto_transaction(cancellable)?;

                let devino = ostree::RepoDevInoCache::new();
                let repodir = Dir::reopen_dir(&repo.dfd_borrow())?;
                let repo_tmp = repodir.open_dir("tmp")?;
                let td = cap_std_ext::cap_tempfile::TempDir::new_in(&repo_tmp)?;

                let rootpath = "root";
                let checkout_mode = if repo.mode() == ostree::RepoMode::Bare {
                    ostree::RepoCheckoutMode::None
                } else {
                    ostree::RepoCheckoutMode::User
                };
                let mut checkout_opts = ostree::RepoCheckoutAtOptions {
                    mode: checkout_mode,
                    overwrite_mode: ostree::RepoCheckoutOverwriteMode::UnionFiles,
                    devino_to_csum_cache: Some(devino.clone()),
                    no_copy_fallback: true,
                    force_copy_zerosized: true,
                    process_whiteouts: false,
                    ..Default::default()
                };
                repo.checkout_at(
                    Some(&checkout_opts),
                    (*td).as_raw_fd(),
                    rootpath,
                    &base_commit,
                    cancellable,
                )
                .context("Checking out base commit")?;

                // Layer all subsequent commits
                checkout_opts.process_whiteouts = true;
                for commit in layer_commits {
                    repo.checkout_at(
                        Some(&checkout_opts),
                        (*td).as_raw_fd(),
                        rootpath,
                        &commit,
                        cancellable,
                    )
                    .with_context(|| format!("Checking out layer {commit}"))?;
                }

                let modifier =
                    ostree::RepoCommitModifier::new(ostree::RepoCommitModifierFlags::CONSUME, None);
                modifier.set_devino_cache(&devino);

                let mt = ostree::MutableTree::new();
                repo.write_dfd_to_mtree(
                    (*td).as_raw_fd(),
                    rootpath,
                    &mt,
                    Some(&modifier),
                    cancellable,
                )
                .context("Writing merged filesystem to mtree")?;

                let merged_root = repo
                    .write_mtree(&mt, cancellable)
                    .context("Writing mtree")?;
                let merged_root = merged_root.downcast::<ostree::RepoFile>().unwrap();
                let merged_commit = repo
                    .write_commit_with_time(
                        None,
                        None,
                        None,
                        Some(&metadata),
                        &merged_root,
                        timestamp,
                        cancellable,
                    )
                    .context("Writing commit")?;
                if !self.no_imgref {
                    repo.transaction_set_ref(None, &ostree_ref, Some(merged_commit.as_str()));
                }
                txn.commit(cancellable)?;

                if !self.disable_gc {
                    let n: u32 = gc_image_layers_impl(repo, cancellable)?;
                    tracing::debug!("pruned {n} layers");
                }

                // Here we re-query state just to run through the same code path,
                // though it'd be cheaper to synthesize it from the data we already have.
                let state = query_image_commit(repo, &merged_commit)?;
                Ok(state)
            },
        )
        .await?;
        Ok(state)
    }
}

/// List all images stored
pub fn list_images(repo: &ostree::Repo) -> Result<Vec<String>> {
    let cancellable = gio::Cancellable::NONE;
    let refs = repo.list_refs_ext(
        Some(IMAGE_PREFIX),
        ostree::RepoListRefsExtFlags::empty(),
        cancellable,
    )?;
    refs.keys()
        .map(|imgname| refescape::unprefix_unescape_ref(IMAGE_PREFIX, imgname))
        .collect()
}

/// Attempt to query metadata for a pulled image; if it is corrupted,
/// the error is printed to stderr and None is returned.
fn try_query_image_ref(
    repo: &ostree::Repo,
    imgref: &ImageReference,
) -> Result<Option<Box<LayeredImageState>>> {
    let ostree_ref = &ref_for_image(imgref)?;
    if let Some(merge_rev) = repo.resolve_rev(ostree_ref, true)? {
        match query_image_commit(repo, merge_rev.as_str()) {
            Ok(r) => Ok(Some(r)),
            Err(e) => {
                eprintln!("error: failed to query image commit: {e}");
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

/// Query metadata for a pulled image.
#[context("Querying image {imgref}")]
pub fn query_image_ref(
    repo: &ostree::Repo,
    imgref: &ImageReference,
) -> Result<Option<Box<LayeredImageState>>> {
    let ostree_ref = &ref_for_image(imgref)?;
    let merge_rev = repo.resolve_rev(ostree_ref, true)?;
    merge_rev
        .map(|r| query_image_commit(repo, r.as_str()))
        .transpose()
}

/// Given detached commit metadata, parse the data that we serialized for a pending update (if any).
fn parse_cached_update(meta: &glib::VariantDict) -> Result<Option<CachedImageUpdate>> {
    // Try to retrieve the manifest digest key from the commit detached metadata.
    let manifest_digest =
        if let Some(d) = meta.lookup::<String>(ImageImporter::CACHED_KEY_MANIFEST_DIGEST)? {
            d
        } else {
            // It's possible that something *else* wrote detached metadata, but without
            // our key; gracefully handle that.
            return Ok(None);
        };
    // If we found the cached manifest digest key, then we must have the manifest and config;
    // otherwise that's an error.
    let manifest = meta.lookup_value(ImageImporter::CACHED_KEY_MANIFEST, None);
    let manifest: oci_image::ImageManifest = manifest
        .as_ref()
        .and_then(|v| v.str())
        .map(serde_json::from_str)
        .transpose()?
        .ok_or_else(|| {
            anyhow!(
                "Expected cached manifest {}",
                ImageImporter::CACHED_KEY_MANIFEST
            )
        })?;
    let config = meta.lookup_value(ImageImporter::CACHED_KEY_CONFIG, None);
    let config: oci_image::ImageConfiguration = config
        .as_ref()
        .and_then(|v| v.str())
        .map(serde_json::from_str)
        .transpose()?
        .ok_or_else(|| {
            anyhow!(
                "Expected cached manifest {}",
                ImageImporter::CACHED_KEY_CONFIG
            )
        })?;
    Ok(Some(CachedImageUpdate {
        manifest,
        config,
        manifest_digest,
    }))
}

/// Query metadata for a pulled image via an OSTree commit digest.
/// The digest must refer to a pulled container image's merge commit.
pub fn query_image_commit(repo: &ostree::Repo, commit: &str) -> Result<Box<LayeredImageState>> {
    let merge_commit = commit.to_string();
    let merge_commit_obj = repo.load_commit(commit)?.0;
    let commit_meta = &merge_commit_obj.child_value(0);
    let commit_meta = &ostree::glib::VariantDict::new(Some(commit_meta));
    let (manifest, manifest_digest) = manifest_data_from_commitmeta(commit_meta)?;
    let configuration = image_config_from_commitmeta(commit_meta)?;
    let mut layers = manifest.layers().iter().cloned();
    // We require a base layer.
    let base_layer = layers.next().ok_or_else(|| anyhow!("No layers found"))?;
    let base_layer = query_layer(repo, base_layer)?;
    let ostree_ref = base_layer.ostree_ref.as_str();
    let base_commit = base_layer
        .commit
        .ok_or_else(|| anyhow!("Missing base image ref {ostree_ref}"))?;
    // If there are more layers after the base, then we're layered.
    let is_layered = layers.count() > 0;

    let detached_commitmeta =
        repo.read_commit_detached_metadata(&merge_commit, gio::Cancellable::NONE)?;
    let detached_commitmeta = detached_commitmeta
        .as_ref()
        .map(|v| glib::VariantDict::new(Some(&v)));
    let cached_update = detached_commitmeta
        .as_ref()
        .map(parse_cached_update)
        .transpose()?
        .flatten();
    let state = Box::new(LayeredImageState {
        base_commit,
        merge_commit,
        is_layered,
        manifest_digest,
        manifest,
        configuration,
        cached_update,
    });
    tracing::debug!("Wrote merge commit {}", state.merge_commit);
    Ok(state)
}

/// Query metadata for a pulled image.
///
/// This is a thin wrapper for [`query_image_ref`] and should
/// be considered deprecated.
// semver-break: Delete this and rename query_image_ref -> query_image
pub fn query_image(
    repo: &ostree::Repo,
    imgref: &OstreeImageReference,
) -> Result<Option<Box<LayeredImageState>>> {
    query_image_ref(repo, &imgref.imgref)
}

fn manifest_for_image(repo: &ostree::Repo, imgref: &ImageReference) -> Result<ImageManifest> {
    let ostree_ref = ref_for_image(imgref)?;
    let rev = repo.require_rev(&ostree_ref)?;
    let (commit_obj, _) = repo.load_commit(rev.as_str())?;
    let commit_meta = &glib::VariantDict::new(Some(&commit_obj.child_value(0)));
    Ok(manifest_data_from_commitmeta(commit_meta)?.0)
}

/// Copy a downloaded image from one repository to another.
#[context("Copying image")]
#[deprecated = "Use copy_as instead"]
// semver-break: Delete this and rename copy_as -> copy
pub async fn copy(
    src_repo: &ostree::Repo,
    dest_repo: &ostree::Repo,
    imgref: &OstreeImageReference,
) -> Result<()> {
    // For historical reasons, this function takes an ostree refernece
    // as input, but the storage only operaties on image references.
    let imgref = &imgref.imgref;
    copy_as(src_repo, imgref, dest_repo, imgref).await
}

/// Copy a downloaded image from one repository to another, while also
/// optionally changing the image reference type.
#[context("Copying image")]
pub async fn copy_as(
    src_repo: &ostree::Repo,
    src_imgref: &ImageReference,
    dest_repo: &ostree::Repo,
    dest_imgref: &ImageReference,
) -> Result<()> {
    let src_ostree_ref = ref_for_image(src_imgref)?;
    let src_commit = src_repo.require_rev(&src_ostree_ref)?;
    let manifest = manifest_for_image(src_repo, src_imgref)?;
    // Create a task to copy each layer, plus the final ref
    let layer_refs = manifest
        .layers()
        .iter()
        .map(ref_for_layer)
        .chain(std::iter::once(Ok(src_commit.to_string())));
    for ostree_ref in layer_refs {
        let ostree_ref = ostree_ref?;
        let src_repo = src_repo.clone();
        let dest_repo = dest_repo.clone();
        crate::tokio_util::spawn_blocking_cancellable_flatten(move |cancellable| -> Result<_> {
            let cancellable = Some(cancellable);
            let srcfd = &format!("file:///proc/self/fd/{}", src_repo.dfd());
            let flags = ostree::RepoPullFlags::MIRROR;
            let opts = glib::VariantDict::new(None);
            let refs = [ostree_ref.as_str()];
            // Some older archives may have bindings, we don't need to verify them.
            opts.insert("disable-verify-bindings", &true);
            opts.insert("refs", &&refs[..]);
            opts.insert("flags", &(flags.bits() as i32));
            let options = opts.to_variant();
            dest_repo.pull_with_options(srcfd, &options, None, cancellable)?;
            Ok(())
        })
        .await?;
    }

    let dest_ostree_ref = ref_for_image(dest_imgref)?;
    dest_repo.set_ref_immediate(
        None,
        &dest_ostree_ref,
        Some(&src_commit),
        gio::Cancellable::NONE,
    )?;

    Ok(())
}

/// Iterate over deployment commits, returning the manifests from
/// commits which point to a container image.
#[context("Listing deployment manifests")]
fn list_container_deployment_manifests(
    repo: &ostree::Repo,
    cancellable: Option<&gio::Cancellable>,
) -> Result<Vec<ImageManifest>> {
    // Gather all refs which start with ostree/0/ or ostree/1/ or rpmostree/base/
    // and create a set of the commits which they reference.
    let commits = OSTREE_BASE_DEPLOYMENT_REFS
        .iter()
        .chain(RPMOSTREE_BASE_REFS)
        .chain(std::iter::once(&BASE_IMAGE_PREFIX))
        .try_fold(
            std::collections::HashSet::new(),
            |mut acc, &p| -> Result<_> {
                let refs = repo.list_refs_ext(
                    Some(p),
                    ostree::RepoListRefsExtFlags::empty(),
                    cancellable,
                )?;
                for (_, v) in refs {
                    acc.insert(v);
                }
                Ok(acc)
            },
        )?;
    // Loop over the commits - if they refer to a container image, add that to our return value.
    let mut r = Vec::new();
    for commit in commits {
        let commit_obj = repo.load_commit(&commit)?.0;
        let commit_meta = &glib::VariantDict::new(Some(&commit_obj.child_value(0)));
        if commit_meta
            .lookup::<String>(META_MANIFEST_DIGEST)?
            .is_some()
        {
            tracing::trace!("Commit {commit} is a container image");
            let manifest = manifest_data_from_commitmeta(commit_meta)?.0;
            r.push(manifest);
        }
    }
    Ok(r)
}

/// Garbage collect unused image layer references.
///
/// This function assumes no transaction is active on the repository.
/// The underlying objects are *not* pruned; that requires a separate invocation
/// of [`ostree::Repo::prune`].
pub fn gc_image_layers(repo: &ostree::Repo) -> Result<u32> {
    gc_image_layers_impl(repo, gio::Cancellable::NONE)
}

#[context("Pruning image layers")]
fn gc_image_layers_impl(
    repo: &ostree::Repo,
    cancellable: Option<&gio::Cancellable>,
) -> Result<u32> {
    let all_images = list_images(repo)?;
    let deployment_commits = list_container_deployment_manifests(repo, cancellable)?;
    let all_manifests = all_images
        .into_iter()
        .map(|img| {
            ImageReference::try_from(img.as_str()).and_then(|ir| manifest_for_image(repo, &ir))
        })
        .chain(deployment_commits.into_iter().map(Ok))
        .collect::<Result<Vec<_>>>()?;
    tracing::debug!("Images found: {}", all_manifests.len());
    let mut referenced_layers = BTreeSet::new();
    for m in all_manifests.iter() {
        for layer in m.layers() {
            referenced_layers.insert(layer.digest().as_str());
        }
    }
    tracing::debug!("Referenced layers: {}", referenced_layers.len());
    let found_layers = repo
        .list_refs_ext(
            Some(LAYER_PREFIX),
            ostree::RepoListRefsExtFlags::empty(),
            cancellable,
        )?
        .into_iter()
        .map(|v| v.0);
    tracing::debug!("Found layers: {}", found_layers.len());
    let mut pruned = 0u32;
    for layer_ref in found_layers {
        let layer_digest = refescape::unprefix_unescape_ref(LAYER_PREFIX, &layer_ref)?;
        if referenced_layers.remove(layer_digest.as_str()) {
            continue;
        }
        pruned += 1;
        tracing::debug!("Pruning: {}", layer_ref.as_str());
        repo.set_ref_immediate(None, layer_ref.as_str(), None, cancellable)?;
    }

    Ok(pruned)
}

#[cfg(feature = "internal-testing-api")]
/// Return how many container blobs (layers) are stored
pub fn count_layer_references(repo: &ostree::Repo) -> Result<u32> {
    let cancellable = gio::Cancellable::NONE;
    let n = repo
        .list_refs_ext(
            Some(LAYER_PREFIX),
            ostree::RepoListRefsExtFlags::empty(),
            cancellable,
        )?
        .len();
    Ok(n as u32)
}

/// Given an image, if it has any non-ostree compatible content, return a suitable
/// warning message.
pub fn image_filtered_content_warning(
    repo: &ostree::Repo,
    image: &ImageReference,
) -> Result<Option<String>> {
    use std::fmt::Write;

    let ostree_ref = ref_for_image(image)?;
    let rev = repo.require_rev(&ostree_ref)?;
    let commit_obj = repo.load_commit(rev.as_str())?.0;
    let commit_meta = &glib::VariantDict::new(Some(&commit_obj.child_value(0)));

    let r = commit_meta
        .lookup::<MetaFilteredData>(META_FILTERED)?
        .filter(|v| !v.is_empty())
        .map(|v| {
            let mut filtered = HashMap::<&String, u32>::new();
            for paths in v.values() {
                for (k, v) in paths {
                    let e = filtered.entry(k).or_default();
                    *e += v;
                }
            }
            let mut buf = "Image contains non-ostree compatible file paths:".to_string();
            for (k, v) in filtered {
                write!(buf, " {k}: {v}").unwrap();
            }
            buf
        });
    Ok(r)
}

/// Remove the specified image reference.  If the image is already
/// not present, this function will successfully perform no operation.
///
/// This function assumes no transaction is active on the repository.
/// The underlying layers are *not* pruned; that requires a separate invocation
/// of [`gc_image_layers`].
#[context("Pruning {img}")]
pub fn remove_image(repo: &ostree::Repo, img: &ImageReference) -> Result<bool> {
    let ostree_ref = &ref_for_image(img)?;
    let found = repo.resolve_rev(ostree_ref, true)?.is_some();
    // Note this API is already idempotent, but we might as well avoid another
    // trip into ostree.
    if found {
        repo.set_ref_immediate(None, ostree_ref, None, gio::Cancellable::NONE)?;
    }
    Ok(found)
}

/// Remove the specified image references.  If an image is not found, further
/// images will be removed, but an error will be returned.
///
/// This function assumes no transaction is active on the repository.
/// The underlying layers are *not* pruned; that requires a separate invocation
/// of [`gc_image_layers`].
pub fn remove_images<'a>(
    repo: &ostree::Repo,
    imgs: impl IntoIterator<Item = &'a ImageReference>,
) -> Result<()> {
    let mut missing = Vec::new();
    for img in imgs.into_iter() {
        let found = remove_image(repo, img)?;
        if !found {
            missing.push(img);
        }
    }
    if !missing.is_empty() {
        let missing = missing.into_iter().fold("".to_string(), |mut a, v| {
            a.push_str(&v.to_string());
            a
        });
        return Err(anyhow::anyhow!("Missing images: {missing}"));
    }
    Ok(())
}

#[derive(Debug, Default)]
struct CompareState {
    verified: BTreeSet<Utf8PathBuf>,
    inode_corrupted: BTreeSet<Utf8PathBuf>,
    unknown_corrupted: BTreeSet<Utf8PathBuf>,
}

impl CompareState {
    fn is_ok(&self) -> bool {
        self.inode_corrupted.is_empty() && self.unknown_corrupted.is_empty()
    }
}

fn compare_file_info(src: &gio::FileInfo, target: &gio::FileInfo) -> bool {
    if src.file_type() != target.file_type() {
        return false;
    }
    if src.size() != target.size() {
        return false;
    }
    for attr in ["unix::uid", "unix::gid", "unix::mode"] {
        if src.attribute_uint32(attr) != target.attribute_uint32(attr) {
            return false;
        }
    }
    true
}

#[context("Querying object inode")]
fn inode_of_object(repo: &ostree::Repo, checksum: &str) -> Result<u64> {
    let repodir = Dir::reopen_dir(&repo.dfd_borrow())?;
    let (prefix, suffix) = checksum.split_at(2);
    let objpath = format!("objects/{}/{}.file", prefix, suffix);
    let metadata = repodir.symlink_metadata(objpath)?;
    Ok(metadata.ino())
}

fn compare_commit_trees(
    repo: &ostree::Repo,
    root: &Utf8Path,
    target: &ostree::RepoFile,
    expected: &ostree::RepoFile,
    exact: bool,
    colliding_inodes: &BTreeSet<u64>,
    state: &mut CompareState,
) -> Result<()> {
    let cancellable = gio::Cancellable::NONE;
    let queryattrs = "standard::name,standard::type";
    let queryflags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;
    let expected_iter = expected.enumerate_children(queryattrs, queryflags, cancellable)?;

    while let Some(expected_info) = expected_iter.next_file(cancellable)? {
        let expected_child = expected_iter.child(&expected_info);
        let name = expected_info.name();
        let name = name.to_str().expect("UTF-8 ostree name");
        let path = Utf8PathBuf::from(format!("{root}{name}"));
        let target_child = target.child(name);
        let target_info = crate::diff::query_info_optional(&target_child, queryattrs, queryflags)
            .context("querying optional to")?;
        let is_dir = matches!(expected_info.file_type(), gio::FileType::Directory);
        if let Some(target_info) = target_info {
            let to_child = target_child
                .downcast::<ostree::RepoFile>()
                .expect("downcast");
            to_child.ensure_resolved()?;
            let from_child = expected_child
                .downcast::<ostree::RepoFile>()
                .expect("downcast");
            from_child.ensure_resolved()?;

            if is_dir {
                let from_contents_checksum = from_child.tree_get_contents_checksum();
                let to_contents_checksum = to_child.tree_get_contents_checksum();
                if from_contents_checksum != to_contents_checksum {
                    let subpath = Utf8PathBuf::from(format!("{}/", path));
                    compare_commit_trees(
                        repo,
                        &subpath,
                        &from_child,
                        &to_child,
                        exact,
                        colliding_inodes,
                        state,
                    )?;
                }
            } else {
                let from_checksum = from_child.checksum();
                let to_checksum = to_child.checksum();
                let matches = if exact {
                    from_checksum == to_checksum
                } else {
                    compare_file_info(&target_info, &expected_info)
                };
                if !matches {
                    let from_inode = inode_of_object(repo, &from_checksum)?;
                    let to_inode = inode_of_object(repo, &to_checksum)?;
                    if colliding_inodes.contains(&from_inode)
                        || colliding_inodes.contains(&to_inode)
                    {
                        state.inode_corrupted.insert(path);
                    } else {
                        state.unknown_corrupted.insert(path);
                    }
                } else {
                    state.verified.insert(path);
                }
            }
        } else {
            eprintln!("Missing {path}");
            state.unknown_corrupted.insert(path);
        }
    }
    Ok(())
}

#[context("Verifying container image state")]
pub(crate) fn verify_container_image(
    sysroot: &SysrootLock,
    imgref: &ImageReference,
    state: &LayeredImageState,
    colliding_inodes: &BTreeSet<u64>,
    verbose: bool,
) -> Result<bool> {
    let cancellable = gio::Cancellable::NONE;
    let repo = &sysroot.repo();
    let merge_commit = state.merge_commit.as_str();
    let merge_commit_root = repo.read_commit(merge_commit, gio::Cancellable::NONE)?.0;
    let merge_commit_root = merge_commit_root
        .downcast::<ostree::RepoFile>()
        .expect("downcast");
    merge_commit_root.ensure_resolved()?;

    // This shouldn't happen anymore
    let config = state
        .configuration
        .as_ref()
        .ok_or_else(|| anyhow!("Missing configuration for image"))?;
    let (commit_layer, _component_layers, remaining_layers) =
        parse_manifest_layout(&state.manifest, config)?;

    let mut comparison_state = CompareState::default();

    let query = |l: &Descriptor| query_layer(repo, l.clone());

    let base_tree = repo
        .read_commit(&state.base_commit, cancellable)?
        .0
        .downcast::<ostree::RepoFile>()
        .expect("downcast");
    println!(
        "Verifying with base ostree layer {}",
        ref_for_layer(commit_layer)?
    );
    compare_commit_trees(
        repo,
        "/".into(),
        &merge_commit_root,
        &base_tree,
        true,
        colliding_inodes,
        &mut comparison_state,
    )?;

    let remaining_layers = remaining_layers
        .into_iter()
        .map(query)
        .collect::<Result<Vec<_>>>()?;

    println!("Image has {} derived layers", remaining_layers.len());

    for layer in remaining_layers.iter().rev() {
        let layer_ref = layer.ostree_ref.as_str();
        let layer_commit = layer
            .commit
            .as_deref()
            .ok_or_else(|| anyhow!("Missing layer {layer_ref}"))?;
        let layer_tree = repo
            .read_commit(layer_commit, cancellable)?
            .0
            .downcast::<ostree::RepoFile>()
            .expect("downcast");
        compare_commit_trees(
            repo,
            "/".into(),
            &merge_commit_root,
            &layer_tree,
            false,
            colliding_inodes,
            &mut comparison_state,
        )?;
    }

    let n_verified = comparison_state.verified.len();
    if comparison_state.is_ok() {
        println!("OK image {imgref} (verified={n_verified})");
        println!();
    } else {
        let n_inode = comparison_state.inode_corrupted.len();
        let n_other = comparison_state.unknown_corrupted.len();
        eprintln!("warning: Found corrupted merge commit");
        eprintln!("  inode clashes: {n_inode}");
        eprintln!("  unknown:       {n_other}");
        eprintln!("  ok:            {n_verified}");
        if verbose {
            eprintln!("Mismatches:");
            for path in comparison_state.inode_corrupted {
                eprintln!("  inode: {path}");
            }
            for path in comparison_state.unknown_corrupted {
                eprintln!("  other: {path}");
            }
        }
        eprintln!();
        return Ok(false);
    }

    Ok(true)
}
