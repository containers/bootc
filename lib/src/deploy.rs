//! # Write deployments merging image with configmap
//!
//! Create a merged filesystem tree with the image and mounted configmaps.

use anyhow::{Context, Result};

use cap_std_ext::cap_std::fs::Dir;
use chrono::DateTime;
use fn_error_context::context;
use ostree::{gio, glib};
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::container::store::PrepareResult;
use ostree_ext::oci_spec;
use ostree_ext::ostree;
use ostree_ext::ostree::Deployment;
use ostree_ext::sysroot::SysrootLock;

use crate::spec::Backend;
use crate::spec::HostSpec;
use crate::spec::ImageReference;

// TODO use https://github.com/ostreedev/ostree-rs-ext/pull/493/commits/afc1837ff383681b947de30c0cefc70080a4f87a
const BASE_IMAGE_PREFIX: &str = "ostree/container/baseimage/bootc";

/// Set on an ostree commit if this is a derived commit
const BOOTC_DERIVED_KEY: &str = "bootc.derived";

/// Variant of HostSpec but required to be filled out
pub(crate) struct RequiredHostSpec<'a> {
    pub(crate) image: &'a ImageReference,
    pub(crate) backend: Backend,
}

/// State of a locally fetched image
pub(crate) struct ImageState {
    pub(crate) backend: Backend,
    pub(crate) manifest_digest: String,
    pub(crate) created: Option<DateTime<chrono::Utc>>,
    pub(crate) version: Option<String>,
    pub(crate) ostree_commit: String,
}

impl<'a> RequiredHostSpec<'a> {
    /// Given a (borrowed) host specification, "unwrap" its internal
    /// options, giving a spec that is required to have a base container image.
    pub(crate) fn from_spec(spec: &'a HostSpec) -> Result<Self> {
        let image = spec
            .image
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing image in specification"))?;
        Ok(Self {
            image,
            backend: spec.backend,
        })
    }
}

impl From<ostree_container::store::LayeredImageState> for ImageState {
    fn from(value: ostree_container::store::LayeredImageState) -> Self {
        let version = value.version().map(|v| v.to_owned());
        let ostree_commit = value.get_commit().to_owned();
        let config = value.configuration.as_ref();
        let labels = config.and_then(crate::status::labels_of_config);
        let created = labels
            .and_then(|l| {
                l.get(oci_spec::image::ANNOTATION_CREATED)
                    .map(|s| s.as_str())
            })
            .and_then(crate::status::try_deserialize_timestamp);
        Self {
            backend: Backend::OstreeContainer,
            manifest_digest: value.manifest_digest,
            created,
            version,
            ostree_commit,
        }
    }
}

impl ImageState {
    /// Fetch the manifest corresponding to this image.  May not be available in all backends.
    pub(crate) fn get_manifest(
        &self,
        repo: &ostree::Repo,
    ) -> Result<Option<ostree_ext::oci_spec::image::ImageManifest>> {
        match self.backend {
            Backend::OstreeContainer => {
                ostree_container::store::query_image_commit(repo, &self.ostree_commit)
                    .map(|v| Some(v.manifest))
            }
            // TODO: Figure out if we can get the OCI manifest from podman
            Backend::Container => Ok(None),
        }
    }
}

/// Wrapper for pulling a container image, wiring up status output.
pub(crate) async fn new_importer(
    repo: &ostree::Repo,
    imgref: &ostree_container::OstreeImageReference,
) -> Result<ostree_container::store::ImageImporter> {
    let config = Default::default();
    let mut imp = ostree_container::store::ImageImporter::new(repo, imgref, config).await?;
    imp.require_bootable();
    Ok(imp)
}

/// Wrapper for pulling a container image, wiring up status output.
#[context("Pulling")]
pub(crate) async fn pull(
    sysroot: &SysrootLock,
    backend: Backend,
    imgref: &ImageReference,
    quiet: bool,
) -> Result<Box<ImageState>> {
    match backend {
        Backend::OstreeContainer => pull_via_ostree(sysroot, imgref, quiet).await,
        Backend::Container => pull_via_podman(sysroot, imgref, quiet).await,
    }
}

/// Wrapper for pulling a container image, wiring up status output.
async fn pull_via_podman(
    sysroot: &SysrootLock,
    imgref: &ImageReference,
    quiet: bool,
) -> Result<Box<ImageState>> {
    let rootfs = &Dir::reopen_dir(&crate::utils::sysroot_fd_borrowed(sysroot))?;
    let fetched_imageid = crate::podman::podman_pull(rootfs, imgref, quiet).await?;
    crate::podman_ostree::commit_image_to_ostree(sysroot, &fetched_imageid)
        .await
        .map(Box::new)
}

async fn pull_via_ostree(
    sysroot: &SysrootLock,
    imgref: &ImageReference,
    quiet: bool,
) -> Result<Box<ImageState>> {
    let repo = &sysroot.repo();
    let imgref = &OstreeImageReference::from(imgref.clone());
    let mut imp = new_importer(repo, imgref).await?;
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => {
            println!("No changes in {} => {}", imgref, c.manifest_digest);
            return Ok(Box::new((*c).into()));
        }
        PrepareResult::Ready(p) => p,
    };
    if let Some(warning) = prep.deprecated_warning() {
        ostree_ext::cli::print_deprecated_warning(warning).await;
    }
    ostree_ext::cli::print_layer_status(&prep);
    let printer = (!quiet).then(|| {
        let layer_progress = imp.request_progress();
        let layer_byte_progress = imp.request_layer_progress();
        tokio::task::spawn(async move {
            ostree_ext::cli::handle_layer_progress_print(layer_progress, layer_byte_progress).await
        })
    });
    let import = imp.import(prep).await;
    if let Some(printer) = printer {
        let _ = printer.await;
    }
    let import = import?;
    if let Some(msg) =
        ostree_container::store::image_filtered_content_warning(repo, &imgref.imgref)?
    {
        eprintln!("{msg}")
    }
    Ok(Box::new((*import).into()))
}

pub(crate) async fn cleanup(sysroot: &SysrootLock) -> Result<()> {
    let repo = sysroot.repo();
    let sysroot = sysroot.sysroot.clone();
    ostree_ext::tokio_util::spawn_blocking_cancellable_flatten(move |cancellable| {
        let cancellable = Some(cancellable);
        let repo = &repo;
        let txn = repo.auto_transaction(cancellable)?;
        let repo = txn.repo();

        // Regenerate our base references.  First, we delete the ones that exist
        for ref_entry in repo
            .list_refs_ext(
                Some(BASE_IMAGE_PREFIX),
                ostree::RepoListRefsExtFlags::NONE,
                cancellable,
            )
            .context("Listing refs")?
            .keys()
        {
            repo.transaction_set_refspec(ref_entry, None);
        }

        // Then, for each deployment which is derived (e.g. has configmaps) we synthesize
        // a base ref to ensure that it's not GC'd.
        for (i, deployment) in sysroot.deployments().into_iter().enumerate() {
            let commit = deployment.csum();
            if let Some(base) = get_base_commit(repo, &commit)? {
                repo.transaction_set_refspec(&format!("{BASE_IMAGE_PREFIX}/{i}"), Some(&base));
            }
        }

        Ok(())
    })
    .await
}

/// If commit is a bootc-derived commit (e.g. has configmaps), return its base.
#[context("Finding base commit")]
pub(crate) fn get_base_commit(repo: &ostree::Repo, commit: &str) -> Result<Option<String>> {
    let commitv = repo.load_commit(commit)?.0;
    let commitmeta = commitv.child_value(0);
    let commitmeta = &glib::VariantDict::new(Some(&commitmeta));
    let r = commitmeta.lookup::<String>(BOOTC_DERIVED_KEY)?;
    Ok(r)
}

#[context("Writing deployment")]
async fn deploy(
    sysroot: &SysrootLock,
    merge_deployment: Option<&Deployment>,
    stateroot: &str,
    image: &ImageState,
    origin: &glib::KeyFile,
) -> Result<()> {
    let stateroot = Some(stateroot);
    // Copy to move into thread
    let cancellable = gio::Cancellable::NONE;
    let _new_deployment = sysroot.stage_tree_with_options(
        stateroot,
        image.ostree_commit.as_str(),
        Some(origin),
        merge_deployment,
        &Default::default(),
        cancellable,
    )?;
    Ok(())
}

/// Stage (queue deployment of) a fetched container image.
#[context("Staging")]
pub(crate) async fn stage(
    sysroot: &SysrootLock,
    stateroot: &str,
    image: &ImageState,
    spec: &RequiredHostSpec<'_>,
) -> Result<()> {
    let merge_deployment = sysroot.merge_deployment(Some(stateroot));
    let origin = glib::KeyFile::new();
    let imgref = OstreeImageReference::from(spec.image.clone());
    origin.set_string(
        "origin",
        ostree_container::deploy::ORIGIN_CONTAINER,
        imgref.to_string().as_str(),
    );
    crate::deploy::deploy(
        sysroot,
        merge_deployment.as_ref(),
        stateroot,
        image,
        &origin,
    )
    .await?;
    crate::deploy::cleanup(sysroot).await?;
    println!("Queued for next boot: {imgref}");
    if let Some(version) = image.version.as_deref() {
        println!("  Version: {version}");
    }
    println!("  Digest: {}", image.manifest_digest);
    ostree_container::deploy::remove_undeployed_images(sysroot).context("Pruning images")?;

    Ok(())
}
