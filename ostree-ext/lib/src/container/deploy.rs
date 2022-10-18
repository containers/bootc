//! Perform initial setup for a container image based system root

use super::store::LayeredImageState;
use super::OstreeImageReference;
use crate::container::store::PrepareResult;
use anyhow::Result;
use fn_error_context::context;
use ostree::glib;

/// The key in the OSTree origin which holds a serialized [`super::OstreeImageReference`].
pub const ORIGIN_CONTAINER: &str = "container-image-reference";

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
    let cancellable = ostree::gio::NONE_CANCELLABLE;
    let options = options.unwrap_or_default();
    let repo = &sysroot.repo().unwrap();
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
                crate::cli::print_deprecated_warning(warning);
            }

            imp.import(prep).await?
        }
    };
    let commit = state.get_commit();
    let origin = glib::KeyFile::new();
    let target_imgref = options.target_imgref.unwrap_or(imgref);
    origin.set_string("origin", ORIGIN_CONTAINER, &target_imgref.to_string());

    if sysroot.booted_deployment().is_some() {
        let opts = ostree::SysrootDeployTreeOpts {
            override_kernel_argv: options.kargs,
            ..Default::default()
        };
        sysroot.stage_tree_with_options(
            Some(stateroot),
            commit,
            Some(&origin),
            merge_deployment.as_ref(),
            &opts,
            cancellable,
        )?;
    } else {
        let deployment = &sysroot.deploy_tree(
            Some(stateroot),
            commit,
            Some(&origin),
            merge_deployment.as_ref(),
            options.kargs.unwrap_or_default(),
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
