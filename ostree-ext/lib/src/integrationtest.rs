//! Module used for integration tests; should not be public.

use std::path::Path;

use crate::container::ocidir;
use anyhow::Result;
use camino::Utf8Path;
use fn_error_context::context;
use gio::prelude::*;
use oci_spec::image as oci_image;
use ostree::gio;

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

/// Create a test fixture in the same way our unit tests does, and print
/// the location of the temporary directory.  Also export a chunked image.
/// Useful for debugging things interactively.
pub(crate) fn create_fixture() -> Result<()> {
    let fixture = crate::fixture::Fixture::new_v1()?;
    let imgref = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(fixture.export_container())
            .map(|v| v.0)
    })?;
    println!("Wrote: {:?}", imgref);
    let path = fixture.into_tempdir().into_path();
    println!("Wrote: {:?}", path);
    Ok(())
}

pub(crate) fn test_ima() -> Result<()> {
    use gvariant::aligned_bytes::TryAsAligned;
    use gvariant::{gv, Marker, Structure};

    let cancellable = gio::NONE_CANCELLABLE;
    let fixture = crate::fixture::Fixture::new_v1()?;

    let config = indoc::indoc! { r#"
    [ req ]
    default_bits = 3048
    distinguished_name = req_distinguished_name
    prompt = no
    string_mask = utf8only
    x509_extensions = myexts
    [ req_distinguished_name ]
    O = Test
    CN = Test key
    emailAddress = example@example.com
    [ myexts ]
    basicConstraints=critical,CA:FALSE
    keyUsage=digitalSignature
    subjectKeyIdentifier=hash
    authorityKeyIdentifier=keyid
    "#};
    std::fs::write(fixture.path.join("genkey.config"), config)?;
    sh_inline::bash_in!(
        &fixture.dir,
        "openssl req -new -nodes -utf8 -sha256 -days 36500 -batch \
        -x509 -config genkey.config \
        -outform DER -out ima.der -keyout privkey_ima.pem &>/dev/null"
    )?;

    let imaopts = crate::ima::ImaOpts {
        algorithm: "sha256".into(),
        key: fixture.path.join("privkey_ima.pem"),
        overwrite: false,
    };
    let rewritten_commit =
        crate::ima::ima_sign(fixture.srcrepo(), fixture.testref(), &imaopts).unwrap();

    let root = fixture
        .srcrepo()
        .read_commit(&rewritten_commit, cancellable)?
        .0;
    let bash = root.resolve_relative_path("/usr/bin/bash");
    let bash = bash.downcast_ref::<ostree::RepoFile>().unwrap();
    let xattrs = bash.xattrs(cancellable).unwrap();
    let v = xattrs.data_as_bytes();
    let v = v.try_as_aligned().unwrap();
    let v = gv!("a(ayay)").cast(v);
    let mut found_ima = false;
    for xattr in v.iter() {
        let k = xattr.to_tuple().0;
        if k != b"security.ima" {
            continue;
        }
        found_ima = true;
        break;
    }
    if !found_ima {
        anyhow::bail!("Failed to find IMA xattr");
    }
    println!("ok IMA");
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
