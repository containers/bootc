//! Perform initial setup for a container image based system root

use std::collections::HashSet;

use super::store::LayeredImageState;
use super::{ImageReference, OstreeImageReference};
use crate::container::store::PrepareResult;
use crate::keyfileext::KeyFileExt;
use crate::sysroot::SysrootLock;
use anyhow::Result;
use fn_error_context::context;
use ostree::glib;

/// The key in the OSTree origin which holds a serialized [`super::OstreeImageReference`].
pub const ORIGIN_CONTAINER: &str = "container-image-reference";

/// The name of the default stateroot.
// xref https://github.com/ostreedev/ostree/issues/2794
pub const STATEROOT_DEFAULT: &str = "default";

/// Options configuring deployment.
#[derive(Debug, Default)]
pub struct DeployOpts<'a> {
    /// Kernel arguments to use.
    pub kargs: Option<&'a [&'a str]>,
    /// Target image reference, as distinct from the source.
    ///
    /// In many cases, one may want a workflow where a system is provisioned from
    /// an image with a specific digest (e.g. `quay.io/example/os@sha256:...) for
    /// reproducibilty.  However, one would want `ostree admin upgrade` to fetch
    /// `quay.io/example/os:latest`.
    ///
    /// To implement this, use this option for the latter `:latest` tag.
    pub target_imgref: Option<&'a OstreeImageReference>,

    /// Configuration for fetching containers.
    pub proxy_cfg: Option<super::store::ImageProxyConfig>,

    /// If true, then no image reference will be written; but there will be refs
    /// for the fetched layers.  This ensures that if the machine is later updated
    /// to a different container image, the fetch process will reuse shared layers, but
    /// it will not be necessary to remove the previous image.
    pub no_imgref: bool,
}

/// Write a container image to an OSTree deployment.
///
/// This API is currently intended for only an initial deployment.
#[context("Performing deployment")]
pub async fn deploy(
    sysroot: &ostree::Sysroot,
    stateroot: &str,
    imgref: &OstreeImageReference,
    options: Option<DeployOpts<'_>>,
) -> Result<Box<LayeredImageState>> {
    let cancellable = ostree::gio::Cancellable::NONE;
    let options = options.unwrap_or_default();
    let repo = &sysroot.repo();
    let merge_deployment = sysroot.merge_deployment(Some(stateroot));
    let mut imp =
        super::store::ImageImporter::new(repo, imgref, options.proxy_cfg.unwrap_or_default())
            .await?;
    if let Some(target) = options.target_imgref {
        imp.set_target(target);
    }
    if options.no_imgref {
        imp.set_no_imgref();
    }
    let state = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(r) => r,
        PrepareResult::Ready(prep) => {
            if let Some(warning) = prep.deprecated_warning() {
                crate::cli::print_deprecated_warning(warning).await;
            }

            imp.import(prep).await?
        }
    };
    let commit = state.get_commit();
    let origin = glib::KeyFile::new();
    let target_imgref = options.target_imgref.unwrap_or(imgref);
    origin.set_string("origin", ORIGIN_CONTAINER, &target_imgref.to_string());

    let opts = ostree::SysrootDeployTreeOpts {
        override_kernel_argv: options.kargs,
        ..Default::default()
    };

    if sysroot.booted_deployment().is_some() {
        sysroot.stage_tree_with_options(
            Some(stateroot),
            commit,
            Some(&origin),
            merge_deployment.as_ref(),
            &opts,
            cancellable,
        )?;
    } else {
        let deployment = &sysroot.deploy_tree_with_options(
            Some(stateroot),
            commit,
            Some(&origin),
            merge_deployment.as_ref(),
            Some(&opts),
            cancellable,
        )?;
        let flags = ostree::SysrootSimpleWriteDeploymentFlags::NONE;
        sysroot.simple_write_deployment(
            Some(stateroot),
            deployment,
            merge_deployment.as_ref(),
            flags,
            cancellable,
        )?;
        sysroot.cleanup(cancellable)?;
    }

    Ok(state)
}

/// Query the container image reference for a deployment
fn deployment_origin_container(
    deploy: &ostree::Deployment,
) -> Result<Option<OstreeImageReference>> {
    let origin = deploy
        .origin()
        .map(|o| o.optional_string("origin", ORIGIN_CONTAINER))
        .transpose()?
        .flatten();
    let r = origin
        .map(|v| OstreeImageReference::try_from(v.as_str()))
        .transpose()?;
    Ok(r)
}

/// Remove all container images which are not the target of a deployment.
/// This acts equivalently to [`super::store::remove_images()`] - the underlying layers
/// are not pruned.
///
/// The set of removed images is returned.
pub fn remove_undeployed_images(sysroot: &SysrootLock) -> Result<Vec<ImageReference>> {
    let repo = &sysroot.repo();
    let deployment_origins: Result<HashSet<_>> = sysroot
        .deployments()
        .into_iter()
        .filter_map(|deploy| {
            deployment_origin_container(&deploy)
                .map(|v| v.map(|v| v.imgref))
                .transpose()
        })
        .collect();
    let deployment_origins = deployment_origins?;
    // TODO add an API that returns ImageReference instead
    let all_images = super::store::list_images(&sysroot.repo())?
        .into_iter()
        .filter_map(|img| ImageReference::try_from(img.as_str()).ok());
    let mut removed = Vec::new();
    for image in all_images {
        if !deployment_origins.contains(&image) {
            super::store::remove_image(repo, &image)?;
            removed.push(image);
        }
    }
    Ok(removed)
}
