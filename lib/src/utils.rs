use anyhow::{Context, Result};
use ostree::glib;
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;

/// Parse an ostree origin file (a keyfile) and extract the targeted
/// container image reference.
pub(crate) fn get_image_origin(
    deployment: &ostree::Deployment,
) -> Result<(glib::KeyFile, Option<OstreeImageReference>)> {
    let origin = deployment
        .origin()
        .ok_or_else(|| anyhow::anyhow!("Missing origin"))?;
    let imgref = origin
        .optional_string("origin", ostree_container::deploy::ORIGIN_CONTAINER)
        .context("Failed to load container image from origin")?
        .map(|v| ostree_container::OstreeImageReference::try_from(v.as_str()))
        .transpose()?;
    Ok((origin, imgref))
}

/// Print the deployment we staged.
pub(crate) fn print_staged(deployment: &ostree::Deployment) -> Result<()> {
    let (_origin, imgref) = get_image_origin(deployment)?;
    let imgref = imgref.ok_or_else(|| {
        anyhow::anyhow!("Internal error: expected a container deployment to be staged")
    })?;
    println!("Queued for next boot: {imgref}");
    Ok(())
}
