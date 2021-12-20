//! Module used for integration tests; should not be public.

use anyhow::{Context, Result};
use camino::Utf8Path;
use fn_error_context::context;
use std::path::Path;

fn has_ostree() -> bool {
    std::path::Path::new("/sysroot/ostree/repo").exists()
}

pub(crate) fn detectenv() -> &'static str {
    match (crate::container_utils::running_in_container(), has_ostree()) {
        (true, true) => "ostree-container",
        (true, false) => "container",
        (false, true) => "ostree",
        (false, false) => "none",
    }
}

fn deserialize_json_path<T: serde::de::DeserializeOwned + Send + 'static>(
    p: impl AsRef<Path>,
) -> Result<T> {
    let p = p.as_ref();
    let ctx = || format!("Parsing {:?}", p);
    let f = std::io::BufReader::new(std::fs::File::open(p).with_context(ctx)?);
    serde_json::from_reader(f).with_context(ctx)
}

fn deserialize_json_blob<T: serde::de::DeserializeOwned + Send + 'static>(
    ocidir: impl AsRef<Utf8Path>,
    desc: &oci_spec::image::Descriptor,
) -> Result<T> {
    let ocidir = ocidir.as_ref();
    let blobpath = desc.digest().replace(':', "/");
    deserialize_json_path(&ocidir.join(&format!("blobs/{}", blobpath)))
}

/// Using `src` as a base, take append `dir` into OCI image.
/// Should only be enabled for testing.
#[cfg(feature = "internal-testing-api")]
#[context("Generating derived oci")]
pub fn generate_derived_oci(src: impl AsRef<Utf8Path>, dir: impl AsRef<Utf8Path>) -> Result<()> {
    let src = src.as_ref();
    let dir = dir.as_ref();
    let index_path = &src.join("index.json");
    let mut idx: oci_spec::image::ImageIndex = deserialize_json_path(index_path)?;
    let mut manifest: oci_spec::image::ImageManifest = {
        let manifest_desc = idx
            .manifests()
            .get(0)
            .ok_or_else(|| anyhow::anyhow!("No manifests found"))?;
        deserialize_json_blob(src, manifest_desc)?
    };
    let mut config: oci_spec::image::ImageConfiguration =
        deserialize_json_blob(src, manifest.config())?;

    let srcdir = &openat::Dir::open(src.as_std_path())?;

    let bw = crate::container::ociwriter::RawLayerWriter::new(srcdir, None)?;
    let mut layer_tar = tar::Builder::new(bw);
    layer_tar.append_dir_all("./", dir.as_std_path())?;
    let bw = layer_tar.into_inner()?;
    let new_layer = bw.complete()?;

    let layers: Vec<_> = manifest
        .layers()
        .iter()
        .cloned()
        .chain(std::iter::once(
            new_layer
                .blob
                .descriptor()
                .media_type(oci_spec::image::MediaType::ImageLayerGzip)
                .build()
                .unwrap(),
        ))
        .collect();
    manifest.set_layers(layers);
    let history: Vec<_> = config
        .history()
        .iter()
        .cloned()
        .chain(std::iter::once(
            oci_spec::image::HistoryBuilder::default()
                .created_by("generate_derived_oci")
                .build()
                .unwrap(),
        ))
        .collect();
    config.set_history(history);
    let diffids: Vec<_> = config
        .rootfs()
        .diff_ids()
        .iter()
        .cloned()
        .chain(std::iter::once(new_layer.uncompressed_sha256))
        .collect();
    config.set_rootfs(
        oci_spec::image::RootFsBuilder::default()
            .diff_ids(diffids)
            .build()
            .unwrap(),
    );
    let new_config_desc = crate::container::ociwriter::write_json_blob(
        srcdir,
        &config,
        oci_spec::image::MediaType::ImageConfig,
    )?
    .build()
    .unwrap();
    manifest.set_config(new_config_desc);

    let new_manifest_desc = crate::container::ociwriter::write_json_blob(
        srcdir,
        &manifest,
        oci_spec::image::MediaType::ImageManifest,
    )?
    .build()
    .unwrap();
    idx.set_manifests(vec![new_manifest_desc]);
    idx.to_file(index_path.as_std_path())?;
    Ok(())
}
