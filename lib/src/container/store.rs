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
use oci_spec::image as oci_image;
use ostree::prelude::{Cast, ToVariant};
use ostree::{gio, glib};
use std::collections::{BTreeMap, HashMap};

/// The ostree ref prefix for blobs.
const LAYER_PREFIX: &str = "ostree/container/blob";
/// The ostree ref prefix for image references.
const IMAGE_PREFIX: &str = "ostree/container/image";

/// The key injected into the merge commit for the manifest digest.
const META_MANIFEST_DIGEST: &str = "ostree.manifest-digest";
/// The key injected into the merge commit with the manifest serialized as JSON.
const META_MANIFEST: &str = "ostree.manifest";

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

/// Context for importing a container image.
pub struct LayeredImageImporter {
    repo: ostree::Repo,
    proxy: ImageProxy,
    imgref: OstreeImageReference,
    proxy_img: OpenedImage,
    ostree_ref: String,
}

/// Result of invoking [`LayeredImageImporter::prepare`].
pub enum PrepareResult {
    /// The image reference is already present; the contained string is the OSTree commit.
    AlreadyPresent(String),
    /// The image needs to be downloaded
    Ready(Box<PreparedImport>),
}

/// A container image layer with associated downloaded-or-not state.
#[derive(Debug)]
pub struct ManifestLayerState {
    layer: oci_image::Descriptor,
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
    /// The previously stored manifest digest.
    pub previous_manifest_digest: Option<String>,
    /// The previously stored image ID.
    pub previous_imageid: Option<String>,
    /// The required base layer.
    pub base_layer: ManifestLayerState,
    /// Any further layers.
    pub layers: Vec<ManifestLayerState>,
}

/// A successful import of a container image.
#[derive(Debug, PartialEq, Eq)]
pub struct CompletedImport {
    /// The ostree ref used for the container image.
    pub ostree_ref: String,
    /// The current commit.
    pub commit: String,
    /// A mapping from layer blob IDs to a count of content filtered out
    /// by toplevel path.
    pub layer_filtered_content: BTreeMap<String, BTreeMap<String, u32>>,
}

// Given a manifest, compute its ostree ref name and cached ostree commit
fn query_layer(repo: &ostree::Repo, layer: oci_image::Descriptor) -> Result<ManifestLayerState> {
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

impl LayeredImageImporter {
    /// Create a new importer.
    pub async fn new(repo: &ostree::Repo, imgref: &OstreeImageReference) -> Result<Self> {
        let proxy = ImageProxy::new().await?;
        let proxy_img = proxy.open_image(&imgref.imgref.to_string()).await?;
        let repo = repo.clone();
        let ostree_ref = ref_for_image(&imgref.imgref)?;
        Ok(LayeredImageImporter {
            repo,
            proxy,
            proxy_img,
            ostree_ref,
            imgref: imgref.clone(),
        })
    }

    /// Determine if there is a new manifest, and if so return its digest.
    #[context("Fetching manifest")]
    pub async fn prepare(&mut self) -> Result<PrepareResult> {
        match &self.imgref.sigverify {
            SignatureSource::ContainerPolicy if skopeo::container_policy_is_default_insecure()? => {
                return Err(anyhow!("containers-policy.json specifies a default of `insecureAcceptAnything`; refusing usage"));
            }
            SignatureSource::OstreeRemote(_) => {
                return Err(anyhow!(
                    "Cannot currently verify layered containers via ostree remote"
                ));
            }
            _ => {}
        }

        let (manifest_digest, manifest_bytes) = self.proxy.fetch_manifest(&self.proxy_img).await?;
        let manifest: oci_image::ImageManifest = serde_json::from_slice(&manifest_bytes)?;
        let new_imageid = manifest.config().digest().as_str();

        // Query for previous stored state
        let (previous_manifest_digest, previous_imageid) = if let Some(merge_commit) =
            self.repo.resolve_rev(&self.ostree_ref, true)?
        {
            let merge_commit_obj = &self.repo.load_commit(merge_commit.as_str())?.0;
            let commit_meta = &merge_commit_obj.child_value(0);
            let commit_meta = &ostree::glib::VariantDict::new(Some(commit_meta));
            let (previous_manifest, previous_digest) = manifest_data_from_commitmeta(commit_meta)?;
            // If the manifest digests match, we're done.
            if previous_digest == manifest_digest {
                return Ok(PrepareResult::AlreadyPresent(merge_commit.to_string()));
            }
            // Failing that, if they have the same imageID, we're also done.
            let previous_imageid = previous_manifest.config().digest().as_str();
            if previous_imageid == new_imageid {
                return Ok(PrepareResult::AlreadyPresent(merge_commit.to_string()));
            }
            (Some(previous_digest), Some(previous_imageid.to_string()))
        } else {
            (None, None)
        };

        let mut layers = manifest.layers().iter().cloned();
        // We require a base layer.
        let base_layer = layers.next().ok_or_else(|| anyhow!("No layers found"))?;
        let base_layer = query_layer(&self.repo, base_layer)?;

        let layers: Result<Vec<_>> = layers
            .map(|layer| -> Result<_> { query_layer(&self.repo, layer) })
            .collect();
        let layers = layers?;

        let imp = PreparedImport {
            manifest,
            manifest_digest,
            previous_manifest_digest,
            previous_imageid,
            base_layer,
            layers,
        };
        Ok(PrepareResult::Ready(Box::new(imp)))
    }

    /// Import a layered container image
    pub async fn import(self, import: Box<PreparedImport>) -> Result<CompletedImport> {
        let proxy = self.proxy;
        // First download the base image (if necessary) - we need the SELinux policy
        // there to label all following layers.
        let base_layer = import.base_layer;
        let base_commit = if let Some(c) = base_layer.commit {
            c
        } else {
            let base_layer_ref = &base_layer.layer;
            let (blob, driver) = super::unencapsulate::fetch_layer_decompress(
                &proxy,
                &self.proxy_img,
                &base_layer.layer,
            )
            .await?;
            let importer = crate::tar::import_tar(&self.repo, blob, None);
            let (commit, driver) = tokio::join!(importer, driver);
            driver?;
            let commit =
                commit.with_context(|| format!("Parsing blob {}", base_layer_ref.digest()))?;
            // TODO support ref writing in tar import
            self.repo.set_ref_immediate(
                None,
                base_layer.ostree_ref.as_str(),
                Some(commit.as_str()),
                gio::NONE_CANCELLABLE,
            )?;
            commit
        };

        let mut layer_commits = Vec::new();
        let mut layer_filtered_content = BTreeMap::new();
        for layer in import.layers {
            if let Some(c) = layer.commit {
                layer_commits.push(c.to_string());
            } else {
                let (blob, driver) = super::unencapsulate::fetch_layer_decompress(
                    &proxy,
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
                let w =
                    crate::tar::write_tar(&self.repo, blob, layer.ostree_ref.as_str(), Some(opts));
                let (r, driver) = tokio::join!(w, driver);
                let r = r.with_context(|| format!("Parsing layer blob {}", layer.digest()))?;
                driver?;
                layer_commits.push(r.commit);
                if !r.filtered.is_empty() {
                    layer_filtered_content.insert(layer.digest().to_string(), r.filtered);
                }
            }
        }

        // We're done with the proxy, make sure it didn't have any errors.
        proxy.finalize().await?;

        let serialized_manifest = serde_json::to_string(&import.manifest)?;
        let mut metadata = HashMap::new();
        metadata.insert(META_MANIFEST_DIGEST, import.manifest_digest.to_variant());
        metadata.insert(META_MANIFEST, serialized_manifest.to_variant());
        metadata.insert(
            "ostree.importer.version",
            env!("CARGO_PKG_VERSION").to_variant(),
        );
        let metadata = metadata.to_variant();

        // Destructure to transfer ownership to thread
        let repo = self.repo;
        let target_ref = self.ostree_ref;
        let (ostree_ref, commit) = crate::tokio_util::spawn_blocking_cancellable(
            move |cancellable| -> Result<(String, String)> {
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
                repo.transaction_set_ref(None, &target_ref, Some(merged_commit.as_str()));
                txn.commit(cancellable)?;
                Ok((target_ref, merged_commit.to_string()))
            },
        )
        .await??;
        Ok(CompletedImport {
            ostree_ref,
            commit,
            layer_filtered_content,
        })
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

/// Copy a downloaded image from one repository to another.
pub async fn copy(
    src_repo: &ostree::Repo,
    dest_repo: &ostree::Repo,
    imgref: &OstreeImageReference,
) -> Result<()> {
    let ostree_ref = ref_for_image(&imgref.imgref)?;
    let rev = src_repo.resolve_rev(&ostree_ref, false)?.unwrap();
    let (commit_obj, _) = src_repo.load_commit(rev.as_str())?;
    let commit_meta = &glib::VariantDict::new(Some(&commit_obj.child_value(0)));
    let (manifest, _) = manifest_data_from_commitmeta(commit_meta)?;
    // Create a task to copy each layer, plus the final ref
    let layer_refs = manifest
        .layers()
        .iter()
        .map(|layer| ref_for_layer(layer))
        .chain(std::iter::once(Ok(ostree_ref)));
    for ostree_ref in layer_refs {
        let ostree_ref = ostree_ref?;
        let src_repo = src_repo.clone();
        let dest_repo = dest_repo.clone();
        crate::tokio_util::spawn_blocking_cancellable(move |cancellable| -> Result<_> {
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
        .await??;
    }
    Ok(())
}

/// Remove the specified images and their corresponding blobs.
pub fn prune_images(_repo: &ostree::Repo, _imgs: &[&str]) -> Result<()> {
    // Most robust approach is to iterate over all known images, load the
    // manifest and build the set of reachable blobs, then compute the set
    // Set(unreachable) = Set(all) - Set(reachable)
    // And remove the unreachable ones.
    unimplemented!()
}
