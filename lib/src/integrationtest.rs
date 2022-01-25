//! Module used for integration tests; should not be public.

use std::path::Path;

use crate::container::ocidir;
use anyhow::Result;
use camino::Utf8Path;
use fn_error_context::context;
use oci_spec::image as oci_image;

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

/// Using `src` as a base, take append `dir` into OCI image.
/// Should only be enabled for testing.
#[cfg(feature = "internal-testing-api")]
#[context("Generating derived oci")]
pub fn generate_derived_oci(src: impl AsRef<Utf8Path>, dir: impl AsRef<Utf8Path>) -> Result<()> {
    use std::rc::Rc;
    let src = src.as_ref();
    let src = Rc::new(openat::Dir::open(src.as_std_path())?);
    let src = ocidir::OciDir::open(src)?;
    let dir = dir.as_ref();
    let mut manifest = src.read_manifest()?;
    let mut config: oci_spec::image::ImageConfiguration = src.read_json_blob(manifest.config())?;

    let bw = src.create_raw_layer(None)?;
    let mut layer_tar = tar::Builder::new(bw);
    layer_tar.append_dir_all("./", dir.as_std_path())?;
    let bw = layer_tar.into_inner()?;
    let new_layer = bw.complete()?;

    manifest.layers_mut().push(
        new_layer
            .blob
            .descriptor()
            .media_type(oci_spec::image::MediaType::ImageLayerGzip)
            .build()
            .unwrap(),
    );
    config.history_mut().push(
        oci_spec::image::HistoryBuilder::default()
            .created_by("generate_derived_oci")
            .build()
            .unwrap(),
    );
    config
        .rootfs_mut()
        .diff_ids_mut()
        .push(new_layer.uncompressed_sha256);
    let new_config_desc = src.write_config(config)?;
    manifest.set_config(new_config_desc);

    src.write_manifest(manifest, oci_image::Platform::default())?;
    Ok(())
}

fn test_proxy_auth() -> Result<()> {
    use containers_image_proxy::ImageProxyConfig;
    let merge = crate::container::merge_default_container_proxy_opts;
    let mut c = ImageProxyConfig::default();
    merge(&mut c)?;
    assert_eq!(c.authfile, None);
    std::fs::create_dir_all("/etc/ostree")?;
    let authpath = Path::new("/etc/ostree/auth.json");
    std::fs::write(authpath, "{}")?;
    let mut c = ImageProxyConfig::default();
    merge(&mut c)?;
    assert_eq!(c.authfile.unwrap().as_path(), authpath,);
    let c = ImageProxyConfig {
        auth_anonymous: true,
        ..Default::default()
    };
    assert_eq!(c.authfile, None);
    std::fs::remove_file(authpath)?;
    let mut c = ImageProxyConfig::default();
    merge(&mut c)?;
    assert_eq!(c.authfile, None);
    Ok(())
}

#[cfg(feature = "internal-testing-api")]
#[context("Running integration tests")]
pub(crate) fn run_tests() -> Result<()> {
    crate::container_utils::require_ostree_container()?;
    // When there's a new integration test to run, add it here.
    test_proxy_auth()?;
    println!("integration tests succeeded.");
    Ok(())
}
