//! Perform initial setup for a container image based system root

use super::OstreeImageReference;
use crate::container::store::PrepareResult;
use anyhow::Result;
use ostree::glib;

/// The key in the OSTree origin which holds a serialized [`super::OstreeImageReference`].
pub const ORIGIN_CONTAINER: &str = "container-image-reference";

async fn pull_idempotent(repo: &ostree::Repo, imgref: &OstreeImageReference) -> Result<String> {
    let mut imp = super::store::LayeredImageImporter::new(repo, imgref).await?;
    match imp.prepare().await? {
        PrepareResult::AlreadyPresent(r) => Ok(r),
        PrepareResult::Ready(prep) => Ok(imp.import(prep).await?.commit),
    }
}

/// Options configuring deployment.
#[derive(Debug, Default)]
pub struct DeployOpts<'a> {
    /// Kernel arguments to use.
    pub kargs: Option<&'a [&'a str]>,
}

/// Write a container image to an OSTree deployment.
///
/// This API is currently intended for only an initial deployment.
pub async fn deploy<'opts>(
    sysroot: &ostree::Sysroot,
    stateroot: &str,
    imgref: &OstreeImageReference,
    options: Option<DeployOpts<'opts>>,
) -> Result<()> {
    let cancellable = ostree::gio::NONE_CANCELLABLE;
    let options = options.unwrap_or_default();
    let repo = &sysroot.repo().unwrap();
    let commit = &pull_idempotent(repo, imgref).await?;
    let origin = glib::KeyFile::new();
    origin.set_string("origin", ORIGIN_CONTAINER, &imgref.to_string());
    let deployment = &sysroot.deploy_tree(
        Some(stateroot),
        commit,
        Some(&origin),
        None,
        options.kargs.unwrap_or_default(),
        cancellable,
    )?;
    let flags = ostree::SysrootSimpleWriteDeploymentFlags::NONE;
    sysroot.simple_write_deployment(Some(stateroot), deployment, None, flags, cancellable)?;
    sysroot.cleanup(cancellable)?;
    Ok(())
}
