//! # Controlling bootc-managed images
//!
//! APIs for operating on container images in the bootc storage.

use anyhow::{Context, Result};
use fn_error_context::context;
use ostree_ext::container::{ImageReference, Transport};

use crate::{cmdutils::CommandRunExt, imgstorage::Storage};

/// The name of the image we push to containers-storage if nothing is specified.
const IMAGE_DEFAULT: &str = "localhost/bootc";

#[context("Listing images")]
pub(crate) async fn list_entrypoint() -> Result<()> {
    let sysroot = crate::cli::get_storage().await?;
    let repo = &sysroot.repo();

    let images = ostree_ext::container::store::list_images(repo).context("Querying images")?;

    println!("# Host images");
    for image in images {
        println!("{image}");
    }
    println!("");

    println!("# Logically bound images");
    let mut listcmd = sysroot.imgstore.new_image_cmd()?;
    listcmd.arg("list");
    listcmd.run()?;

    Ok(())
}

/// Implementation of `bootc image push-to-storage`.
#[context("Pushing image")]
pub(crate) async fn push_entrypoint(source: Option<&str>, target: Option<&str>) -> Result<()> {
    let transport = Transport::ContainerStorage;
    let sysroot = crate::cli::get_storage().await?;

    let repo = &sysroot.repo();

    // If the target isn't specified, push to containers-storage + our default image
    let target = if let Some(target) = target {
        ImageReference {
            transport,
            name: target.to_owned(),
        }
    } else {
        ImageReference {
            transport: Transport::ContainerStorage,
            name: IMAGE_DEFAULT.to_string(),
        }
    };

    // If the source isn't specified, we use the booted image
    let source = if let Some(source) = source {
        ImageReference::try_from(source).context("Parsing source image")?
    } else {
        let status = crate::status::get_status_require_booted(&sysroot)?;
        // SAFETY: We know it's booted
        let booted = status.2.status.booted.unwrap();
        let booted_image = booted.image.unwrap().image;
        ImageReference {
            transport: Transport::try_from(booted_image.transport.as_str()).unwrap(),
            name: booted_image.image,
        }
    };
    let mut opts = ostree_ext::container::store::ExportToOCIOpts::default();
    opts.progress_to_stdout = true;
    println!("Copying local image {source} to {target} ...");
    let r = ostree_ext::container::store::export(repo, &source, &target, Some(opts)).await?;

    println!("Pushed: {target} {r}");
    Ok(())
}

/// Thin wrapper for invoking `podman image <X>` but set up for our internal
/// image store (as distinct from /var/lib/containers default).
pub(crate) async fn imgcmd_entrypoint(
    storage: &Storage,
    arg: &str,
    args: &[std::ffi::OsString],
) -> std::result::Result<(), anyhow::Error> {
    let mut cmd = storage.new_image_cmd()?;
    cmd.arg(arg);
    cmd.args(args);
    cmd.run()
}
