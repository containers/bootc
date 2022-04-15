//! APIs for storing (layered) container images as OSTree commits
//!
//! # Extension of encapsulation support
//!
//! This code supports ingesting arbitrary layered container images from an ostree-exported
//! base.  See [`encapsulate`][`super::encapsulate()`] for more information on encaspulation of images.

use super::*;
use crate::refescape;
use anyhow::{anyhow, Context};
use containers_image_proxy::{ImageProxy, OpenedImage};
use fn_error_context::context;
use oci_spec::image::{self as oci_image, Descriptor, History, ImageConfiguration, ImageManifest};
use ostree::prelude::{Cast, ToVariant};
use ostree::{gio, glib};
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

/// The key injected into the merge commit for the manifest digest.
const META_MANIFEST_DIGEST: &str = "ostree.manifest-digest";
/// The key injected into the merge commit with the manifest serialized as JSON.
const META_MANIFEST: &str = "ostree.manifest";
/// The key injected into the merge commit with the image configuration serialized as JSON.
const META_CONFIG: &str = "ostree.container.image-config";
/// Value of type `a{sa{su}}` containing number of filtered out files
pub const META_FILTERED: &str = "ostree.tar-filtered";
/// The type used to store content filtering information with `META_FILTERED`.
pub type MetaFilteredData = HashMap<String, HashMap<String, u32>>;

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
}

/// Context for importing a container image.
#[derive(Debug)]
pub struct ImageImporter {
    repo: ostree::Repo,
    pub(crate) proxy: ImageProxy,
    imgref: OstreeImageReference,
    target_imgref: Option<OstreeImageReference>,
    pub(crate) proxy_img: OpenedImage,

    layer_progress: Option<Sender<ImportProgress>>,
}

/// Result of invoking [`LayeredImageImporter::prepare`].
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
    /// Iterate over all layers; the ostree split object layers, the commit layer, and any non-ostree layers.
    pub fn all_layers(&self) -> impl Iterator<Item = &ManifestLayerState> {
        self.ostree_layers
            .iter()
            .chain(std::iter::once(&self.ostree_commit_layer))
            .chain(self.layers.iter())
    }

    /// Iterate over all layers paired with their history entry.
    /// An error will be returned if the history does not cover all entries.
    pub fn layers_with_history(
        &self,
    ) -> impl Iterator<Item = Result<(&ManifestLayerState, &History)>> {
        // FIXME use .filter(|h| h.empty_layer.unwrap_or_default()) after https://github.com/containers/oci-spec-rs/pull/100 lands.
        let truncated = std::iter::once(Err(anyhow::anyhow!("Truncated history")));
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
    manifest: &'a ImageManifest,
    config: &ImageConfiguration,
    diffid: &str,
) -> Result<&'a Descriptor> {
    let idx = config
        .rootfs()
        .diff_ids()
        .iter()
        .position(|x| x.as_str() == diffid)
        .ok_or_else(|| anyhow!("Missing {} {}", OSTREE_DIFFID_LABEL, diffid))?;
    manifest.layers().get(idx).ok_or_else(|| {
        anyhow!(
            "diffid position {} exceeds layer count {}",
            idx,
            manifest.layers().len()
        )
    })
}

pub(crate) fn ostree_layer<'a>(
    manifest: &'a ImageManifest,
    config: &ImageConfiguration,
) -> Result<&'a Descriptor> {
    let label = crate::container::OSTREE_DIFFID_LABEL;
    let config_labels = config.config().as_ref().and_then(|c| c.labels().as_ref());
    let diffid = config_labels.and_then(|labels| labels.get(label));
    // For backwards compatibility, if there's only 1 layer, don't require the label.
    // This can be dropped when we drop format version 0 support.
    let r = if let Some(diffid) = diffid {
        layer_from_diffid(manifest, config, diffid.as_str())?
    } else {
        &manifest.layers()[0]
    };
    Ok(r)
}

impl ImageImporter {
    /// Create a new importer.
    pub async fn new(
        repo: &ostree::Repo,
        imgref: &OstreeImageReference,
        mut config: ImageProxyConfig,
    ) -> Result<Self> {
        // Apply our defaults to the proxy config
        merge_default_container_proxy_opts(&mut config)?;
        let proxy = ImageProxy::new_with_config(config).await?;
        let proxy_img = proxy.open_image(&imgref.imgref.to_string()).await?;
        let repo = repo.clone();
        Ok(ImageImporter {
            repo,
            proxy,
            proxy_img,
            target_imgref: None,
            imgref: imgref.clone(),
            layer_progress: None,
        })
    }

    /// Write cached data as if the image came from this source.
    pub fn set_target(&mut self, target: &OstreeImageReference) {
        self.target_imgref = Some(target.clone())
    }

    /// Determine if there is a new manifest, and if so return its digest.
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

        let (previous_manifest_digest, previous_imageid) =
            if let Some(previous_state) = query_image(&self.repo, &self.imgref)? {
                // If the manifest digests match, we're done.
                if previous_state.manifest_digest == manifest_digest {
                    return Ok(PrepareResult::AlreadyPresent(previous_state));
                }
                // Failing that, if they have the same imageID, we're also done.
                let previous_imageid = previous_state.manifest.config().digest().as_str();
                if previous_imageid == new_imageid {
                    return Ok(PrepareResult::AlreadyPresent(previous_state));
                }
                (
                    Some(previous_state.manifest_digest),
                    Some(previous_imageid.to_string()),
                )
            } else {
                (None, None)
            };

        let config = self.proxy.fetch_config(&self.proxy_img).await?;

        let commit_layer_digest = ostree_layer(&manifest, &config)?.digest();

        let mut component_layers = Vec::new();
        let mut commit_layer = None;
        let mut remaining_layers = Vec::new();
        let query = |l: &Descriptor| query_layer(&self.repo, l.clone());
        for layer in manifest.layers() {
            if layer.digest() == commit_layer_digest {
                commit_layer = Some(query(layer)?);
            } else if commit_layer.is_none() {
                component_layers.push(query(layer)?);
            } else {
                remaining_layers.push(query(layer)?);
            }
        }
        let commit_layer = commit_layer.ok_or_else(|| {
            anyhow!(
                "Image does not contain ostree-exported layer {}",
                commit_layer_digest
            )
        })?;

        let imp = PreparedImport {
            manifest,
            manifest_digest,
            config,
            previous_manifest_digest,
            previous_imageid,
            ostree_layers: component_layers,
            ostree_commit_layer: commit_layer,
            layers: remaining_layers,
        };
        Ok(PrepareResult::Ready(Box::new(imp)))
    }

    /// Extract the base ostree commit.
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

        for layer in import.ostree_layers.iter_mut() {
            if layer.commit.is_some() {
                continue;
            }
            if let Some(p) = self.layer_progress.as_ref() {
                p.send(ImportProgress::OstreeChunkStarted(layer.layer.clone()))
                    .await?;
            }
            let (blob, driver) =
                fetch_layer_decompress(&mut self.proxy, &self.proxy_img, &layer.layer).await?;
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
                });
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
                &import.ostree_commit_layer.layer,
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
        self.unencapsulate_base(&mut prep, false).await?;
        let ostree_commit = prep.ostree_commit_layer.commit.unwrap();
        let image_digest = prep.manifest_digest;
        Ok(Import {
            ostree_commit,
            image_digest,
        })
    }

    /// Import a layered container image
    pub async fn import(
        mut self,
        mut import: Box<PreparedImport>,
    ) -> Result<Box<LayeredImageState>> {
        // First download all layers for the base image (if necessary) - we need the SELinux policy
        // there to label all following layers.
        self.unencapsulate_base(&mut import, true).await?;
        let mut proxy = self.proxy;
        let target_imgref = self.target_imgref.as_ref().unwrap_or(&self.imgref);
        let base_commit = import.ostree_commit_layer.commit.clone().unwrap();

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
                    &self.proxy_img,
                    &layer.layer,
                )
                .await?;
                // An important aspect of this is that we SELinux label the derived layers using
                // the base policy.
                let opts = crate::tar::WriteTarOptions {
                    base: Some(base_commit.clone()),
                    selinux: true,
                };
                let r =
                    crate::tar::write_tar(&self.repo, blob, layer.ostree_ref.as_str(), Some(opts));
                let r = super::unencapsulate::join_fetch(r, driver)
                    .await
                    .with_context(|| format!("Parsing layer blob {}", layer.digest()))?;
                layer_commits.push(r.commit);
                if !r.filtered.is_empty() {
                    let filtered = HashMap::from_iter(r.filtered.into_iter());
                    layer_filtered_content.insert(layer.digest().to_string(), filtered);
                }
                if let Some(p) = self.layer_progress.as_ref() {
                    p.send(ImportProgress::DerivedLayerCompleted(layer.layer.clone()))
                        .await?;
                }
            }
        }

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

        // Destructure to transfer ownership to thread
        let repo = self.repo;
        let imgref = self.target_imgref.unwrap_or(self.imgref);
        let state = crate::tokio_util::spawn_blocking_cancellable_flatten(
            move |cancellable| -> Result<Box<LayeredImageState>> {
                let cancellable = Some(cancellable);
                let repo = &repo;
                let txn = repo.auto_transaction(cancellable)?;
                let (base_commit_tree, _) = repo.read_commit(&base_commit, cancellable)?;
                let base_commit_tree = base_commit_tree.downcast::<ostree::RepoFile>().unwrap();
                let base_contents_obj = base_commit_tree.tree_get_contents_checksum().unwrap();
                let base_metadata_obj = base_commit_tree.tree_get_metadata_checksum().unwrap();
                let mt = ostree::MutableTree::from_checksum(
                    repo,
                    &base_contents_obj,
                    &base_metadata_obj,
                );
                // Layer all subsequent commits
                for commit in layer_commits {
                    let (layer_tree, _) = repo.read_commit(&commit, cancellable)?;
                    repo.write_directory_to_mtree(&layer_tree, &mt, None, cancellable)?;
                }

                let merged_root = repo.write_mtree(&mt, cancellable)?;
                let merged_root = merged_root.downcast::<ostree::RepoFile>().unwrap();
                let merged_commit = repo.write_commit(
                    None,
                    None,
                    None,
                    Some(&metadata),
                    &merged_root,
                    cancellable,
                )?;
                repo.transaction_set_ref(None, &ostree_ref, Some(merged_commit.as_str()));
                txn.commit(cancellable)?;
                // Here we re-query state just to run through the same code path,
                // though it'd be cheaper to synthesize it from the data we already have.
                let state = query_image(repo, &imgref)?.unwrap();
                Ok(state)
            },
        )
        .await?;
        Ok(state)
    }
}

/// List all images stored
pub fn list_images(repo: &ostree::Repo) -> Result<Vec<String>> {
    let cancellable = gio::NONE_CANCELLABLE;
    let refs = repo.list_refs_ext(
        Some(IMAGE_PREFIX),
        ostree::RepoListRefsExtFlags::empty(),
        cancellable,
    )?;
    refs.keys()
        .map(|imgname| refescape::unprefix_unescape_ref(IMAGE_PREFIX, imgname))
        .collect()
}

/// Query metadata for a pulled image.
pub fn query_image_ref(
    repo: &ostree::Repo,
    imgref: &ImageReference,
) -> Result<Option<Box<LayeredImageState>>> {
    let ostree_ref = &ref_for_image(imgref)?;
    let merge_rev = repo.resolve_rev(ostree_ref, true)?;
    let (merge_commit, merge_commit_obj) = if let Some(r) = merge_rev {
        (r.to_string(), repo.load_commit(r.as_str())?.0)
    } else {
        return Ok(None);
    };
    let commit_meta = &merge_commit_obj.child_value(0);
    let commit_meta = &ostree::glib::VariantDict::new(Some(commit_meta));
    let (manifest, manifest_digest) = manifest_data_from_commitmeta(commit_meta)?;
    let configuration = image_config_from_commitmeta(commit_meta)?;
    let mut layers = manifest.layers().iter().cloned();
    // We require a base layer.
    let base_layer = layers.next().ok_or_else(|| anyhow!("No layers found"))?;
    let base_layer = query_layer(repo, base_layer)?;
    let base_commit = base_layer
        .commit
        .ok_or_else(|| anyhow!("Missing base image ref"))?;
    // If there are more layers after the base, then we're layered.
    let is_layered = layers.count() > 0;
    let state = Box::new(LayeredImageState {
        base_commit,
        merge_commit,
        is_layered,
        manifest_digest,
        manifest,
        configuration,
    });
    tracing::debug!(state = ?state);
    Ok(Some(state))
}

/// Query metadata for a pulled image.
///
/// This is a thin wrapper for [`query_image_ref`] and should
/// be considered deprecated.
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
pub async fn copy(
    src_repo: &ostree::Repo,
    dest_repo: &ostree::Repo,
    imgref: &OstreeImageReference,
) -> Result<()> {
    let ostree_ref = ref_for_image(&imgref.imgref)?;
    let manifest = manifest_for_image(src_repo, &imgref.imgref)?;
    // Create a task to copy each layer, plus the final ref
    let layer_refs = manifest
        .layers()
        .iter()
        .map(ref_for_layer)
        .chain(std::iter::once(Ok(ostree_ref)));
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
    Ok(())
}

/// Garbage collect unused image layer references.
///
/// This function assumes no transaction is active on the repository.
/// The underlying objects are *not* pruned; that requires a separate invocation
/// of [`ostree::Repo::prune`].
pub fn gc_image_layers(repo: &ostree::Repo) -> Result<u32> {
    let cancellable = gio::NONE_CANCELLABLE;
    let all_images = list_images(repo)?;
    let all_manifests = all_images
        .into_iter()
        .map(|img| {
            ImageReference::try_from(img.as_str()).and_then(|ir| manifest_for_image(repo, &ir))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut referenced_layers = BTreeSet::new();
    for m in all_manifests.iter() {
        for layer in m.layers() {
            referenced_layers.insert(layer.digest().as_str());
        }
    }
    let found_layers = repo
        .list_refs_ext(
            Some(LAYER_PREFIX),
            ostree::RepoListRefsExtFlags::empty(),
            cancellable,
        )?
        .into_iter()
        .map(|v| v.0);
    let mut pruned = 0u32;
    for layer_ref in found_layers {
        let layer_digest = refescape::unprefix_unescape_ref(LAYER_PREFIX, &layer_ref)?;
        if referenced_layers.remove(layer_digest.as_str()) {
            continue;
        }
        pruned += 1;
        repo.set_ref_immediate(None, layer_ref.as_str(), None, cancellable)?;
    }

    Ok(pruned)
}

#[context("Pruning {}", image)]
fn prune_image(repo: &ostree::Repo, image: &ImageReference) -> Result<()> {
    let ostree_ref = &ref_for_image(image)?;

    if repo.resolve_rev(ostree_ref, true)?.is_none() {
        anyhow::bail!("No such image");
    }
    repo.set_ref_immediate(None, ostree_ref, None, gio::NONE_CANCELLABLE)?;
    Ok(())
}

/// Remove the specified image references.
///
/// This function assumes no transaction is active on the repository.
/// The underlying layers are *not* pruned; that requires a separate invocation
/// of [`gc_image_layers`].
pub fn remove_images<'a>(
    repo: &ostree::Repo,
    imgs: impl IntoIterator<Item = &'a ImageReference>,
) -> Result<()> {
    for img in imgs.into_iter() {
        prune_image(repo, img)?;
    }
    Ok(())
}
