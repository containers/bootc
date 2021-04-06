//! APIs for extracting OSTree commits from container images

use std::io::Write;

use super::Result;
use anyhow::{anyhow, Context};
use fn_error_context::context;
use oci_distribution::manifest::OciDescriptor;

/// The result of an import operation
#[derive(Debug)]
pub struct Import {
    /// The ostree commit that was imported
    pub ostree_commit: String,
    /// The image digest retrieved
    pub image_digest: String,
}

#[context("Fetching layer descriptor")]
async fn fetch_layer_descriptor(
    client: &mut oci_distribution::Client,
    image_ref: &oci_distribution::Reference,
) -> Result<(String, OciDescriptor)> {
    let (manifest, digest) = client.pull_manifest(image_ref).await?;
    let mut layers = manifest.layers;
    let orig_layer_count = layers.len();
    layers.retain(|layer| {
        matches!(
            layer.media_type.as_str(),
            super::oci::DOCKER_TYPE_LAYER | oci_distribution::manifest::IMAGE_LAYER_GZIP_MEDIA_TYPE
        )
    });
    let n = layers.len();

    if let Some(layer) = layers.into_iter().next() {
        if n > 1 {
            Err(anyhow!("Expected 1 layer, found {}", n))
        } else {
            Ok((digest, layer))
        }
    } else {
        Err(anyhow!("No layers found (orig: {})", orig_layer_count))
    }
}

#[allow(unsafe_code)]
#[context("Importing {}", image_ref)]
async fn import_impl(repo: &ostree::Repo, image_ref: &str) -> Result<Import> {
    let image_ref: oci_distribution::Reference = image_ref.parse()?;
    let client = &mut oci_distribution::Client::default();
    let auth = &oci_distribution::secrets::RegistryAuth::Anonymous;
    client
        .auth(
            &image_ref,
            auth,
            &oci_distribution::secrets::RegistryOperation::Pull,
        )
        .await?;
    let (image_digest, layer) = fetch_layer_descriptor(client, &image_ref).await?;

    let req = client
        .request_layer(&image_ref, &layer.digest)
        .await?
        .bytes_stream();
    let (pipein, mut pipeout) = os_pipe::pipe()?;
    let copier = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let req = futures::executor::block_on_stream(req);
        for v in req {
            let v = v.map_err(anyhow::Error::msg).context("Writing buf")?;
            pipeout.write_all(&v)?;
        }
        Ok(())
    });
    let repo = repo.clone();
    let import = tokio::task::spawn_blocking(move || {
        let gz = flate2::read::GzDecoder::new(pipein);
        crate::tar::import_tar(&repo, gz)
    });
    let (import_res, copy_res) = tokio::join!(import, copier);
    copy_res??;
    let ostree_commit = import_res??;

    Ok(Import {
        ostree_commit,
        image_digest,
    })
}

/// Download and import the referenced container
pub async fn import<I: AsRef<str>>(repo: &ostree::Repo, image_ref: I) -> Result<Import> {
    Ok(import_impl(repo, image_ref.as_ref()).await?)
}
