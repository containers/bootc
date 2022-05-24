use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std::fs::{Dir, DirBuilder};
use once_cell::sync::Lazy;
use ostree::cap_std;
use ostree_ext::chunking::ObjectMetaSized;
use ostree_ext::container::store;
use ostree_ext::container::{
    Config, ExportOpts, ImageReference, OstreeImageReference, SignatureSource, Transport,
};
use ostree_ext::prelude::FileExt;
use ostree_ext::tar::TarImportOptions;
use ostree_ext::{gio, glib};
use sh_inline::bash_in;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, BufWriter};
use std::os::unix::fs::DirBuilderExt;
use std::process::Command;

use ostree_ext::fixture::{FileDef, Fixture, CONTENTS_CHECKSUM_V0};

const EXAMPLE_TAR_LAYER: &[u8] = include_bytes!("fixtures/hlinks.tar.gz");
const TEST_REGISTRY_DEFAULT: &str = "localhost:5000";

fn assert_err_contains<T>(r: Result<T>, s: impl AsRef<str>) {
    let s = s.as_ref();
    let msg = format!("{:#}", r.err().expect("Expecting an error"));
    if !msg.contains(s) {
        panic!(r#"Error message "{}" did not contain "{}""#, msg, s);
    }
}

static TEST_REGISTRY: Lazy<String> = Lazy::new(|| match std::env::var_os("TEST_REGISTRY") {
    Some(t) => t.to_str().unwrap().to_owned(),
    None => TEST_REGISTRY_DEFAULT.to_string(),
});

// This is mostly just sanity checking these functions are publicly accessible
#[test]
fn test_cli_fns() -> Result<()> {
    let fixture = Fixture::new_v1()?;
    let srcpath = fixture.path.join("src/repo");
    let srcrepo_parsed = ostree_ext::cli::parse_repo(srcpath.as_str()).unwrap();
    assert_eq!(srcrepo_parsed.mode(), fixture.srcrepo().mode());

    let ir =
        ostree_ext::cli::parse_imgref("ostree-unverified-registry:quay.io/examplens/exampleos")
            .unwrap();
    assert_eq!(ir.imgref.transport, Transport::Registry);

    let ir = ostree_ext::cli::parse_base_imgref("docker://quay.io/examplens/exampleos").unwrap();
    assert_eq!(ir.transport, Transport::Registry);
    Ok(())
}

#[tokio::test]
async fn test_tar_import_empty() -> Result<()> {
    let fixture = Fixture::new_v1()?;
    let r = ostree_ext::tar::import_tar(fixture.destrepo(), tokio::io::empty(), None).await;
    assert_err_contains(r, "Commit object not found");
    Ok(())
}

#[tokio::test]
async fn test_tar_export_reproducible() -> Result<()> {
    let fixture = Fixture::new_v1()?;
    let (_, rev) = fixture
        .srcrepo()
        .read_commit(fixture.testref(), gio::NONE_CANCELLABLE)?;
    let export1 = {
        let mut h = openssl::hash::Hasher::new(openssl::hash::MessageDigest::sha256())?;
        ostree_ext::tar::export_commit(fixture.srcrepo(), rev.as_str(), &mut h, None)?;
        h.finish()?
    };
    // Artificial delay to flush out mtimes (one second granularity baseline, plus another 100ms for good measure).
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let export2 = {
        let mut h = openssl::hash::Hasher::new(openssl::hash::MessageDigest::sha256())?;
        ostree_ext::tar::export_commit(fixture.srcrepo(), rev.as_str(), &mut h, None)?;
        h.finish()?
    };
    assert_eq!(*export1, *export2);
    Ok(())
}

#[tokio::test]
async fn test_tar_import_signed() -> Result<()> {
    let fixture = Fixture::new_v1()?;
    let test_tar = fixture.export_tar()?;

    let rev = fixture.srcrepo().require_rev(fixture.testref())?;
    let (commitv, _) = fixture.srcrepo().load_commit(rev.as_str())?;
    assert_eq!(
        ostree::commit_get_content_checksum(&commitv)
            .unwrap()
            .as_str(),
        CONTENTS_CHECKSUM_V0
    );

    // Verify we fail with an unknown remote.
    let src_tar = tokio::fs::File::from_std(fixture.dir.open(test_tar)?.into_std());
    let r = ostree_ext::tar::import_tar(
        fixture.destrepo(),
        src_tar,
        Some(TarImportOptions {
            remote: Some("nosuchremote".to_string()),
        }),
    )
    .await;
    assert_err_contains(r, r#"Remote "nosuchremote" not found"#);

    // Test a remote, but without a key
    let opts = glib::VariantDict::new(None);
    opts.insert("gpg-verify", &true);
    opts.insert("custom-backend", &"ostree-rs-ext");
    fixture
        .destrepo()
        .remote_add("myremote", None, Some(&opts.end()), gio::NONE_CANCELLABLE)?;
    let src_tar = tokio::fs::File::from_std(fixture.dir.open(test_tar)?.into_std());
    let r = ostree_ext::tar::import_tar(
        fixture.destrepo(),
        src_tar,
        Some(TarImportOptions {
            remote: Some("myremote".to_string()),
        }),
    )
    .await;
    assert_err_contains(r, r#"Can't check signature: public key not found"#);

    // And signed correctly
    bash_in!(&fixture.dir,
        "ostree --repo=dest/repo remote gpg-import --stdin myremote < src/gpghome/key1.asc >/dev/null",
    )?;
    let src_tar = tokio::fs::File::from_std(fixture.dir.open(test_tar)?.into_std());
    let imported = ostree_ext::tar::import_tar(
        fixture.destrepo(),
        src_tar,
        Some(TarImportOptions {
            remote: Some("myremote".to_string()),
        }),
    )
    .await?;
    let (commitdata, state) = fixture.destrepo().load_commit(&imported)?;
    assert_eq!(
        CONTENTS_CHECKSUM_V0,
        ostree::commit_get_content_checksum(&commitdata)
            .unwrap()
            .as_str()
    );
    assert_eq!(state, ostree::RepoCommitState::NORMAL);

    // Drop the commit metadata, and verify that import fails
    fixture.clear_destrepo()?;
    let nometa = "test-no-commitmeta.tar";
    let srcf = fixture.dir.open(test_tar)?;
    let destf = fixture.dir.create(nometa)?;
    tokio::task::spawn_blocking(move || -> Result<_> {
        let src = BufReader::new(srcf);
        let f = BufWriter::new(destf);
        ostree_ext::tar::update_detached_metadata(src, f, None, gio::NONE_CANCELLABLE).unwrap();
        Ok(())
    })
    .await??;
    let src_tar = tokio::fs::File::from_std(fixture.dir.open(nometa)?.into_std());
    let r = ostree_ext::tar::import_tar(
        fixture.destrepo(),
        src_tar,
        Some(TarImportOptions {
            remote: Some("myremote".to_string()),
        }),
    )
    .await;
    assert_err_contains(r, "Expected commitmeta object");

    // Now inject garbage into the commitmeta by flipping some bits in the signature
    let rev = fixture.srcrepo().require_rev(fixture.testref())?;
    let commitmeta = fixture
        .srcrepo()
        .read_commit_detached_metadata(&rev, gio::NONE_CANCELLABLE)?
        .unwrap();
    let mut commitmeta = Vec::from(&*commitmeta.data_as_bytes());
    let len = commitmeta.len() / 2;
    let last = commitmeta.get_mut(len).unwrap();
    (*last) = last.wrapping_add(1);

    let srcf = fixture.dir.open(test_tar)?;
    let destf = fixture.dir.create(nometa)?;
    tokio::task::spawn_blocking(move || -> Result<_> {
        let src = BufReader::new(srcf);
        let f = BufWriter::new(destf);
        ostree_ext::tar::update_detached_metadata(src, f, Some(&commitmeta), gio::NONE_CANCELLABLE)
            .unwrap();
        Ok(())
    })
    .await??;
    let src_tar = tokio::fs::File::from_std(fixture.dir.open(nometa)?.into_std());
    let r = ostree_ext::tar::import_tar(
        fixture.destrepo(),
        src_tar,
        Some(TarImportOptions {
            remote: Some("myremote".to_string()),
        }),
    )
    .await;
    assert_err_contains(r, "BAD signature");

    Ok(())
}

#[derive(Debug)]
struct TarExpected {
    path: &'static str,
    etype: tar::EntryType,
    mode: u32,
}

impl Into<TarExpected> for &(&'static str, tar::EntryType, u32) {
    fn into(self) -> TarExpected {
        TarExpected {
            path: self.0,
            etype: self.1,
            mode: self.2,
        }
    }
}

fn validate_tar_expected<T: std::io::Read>(
    format_version: u32,
    t: tar::Entries<T>,
    expected: impl IntoIterator<Item = TarExpected>,
) -> Result<()> {
    let mut expected: HashMap<&'static str, TarExpected> =
        expected.into_iter().map(|exp| (exp.path, exp)).collect();
    let entries = t.map(|e| e.unwrap());
    let mut seen_paths = HashSet::new();
    // Verify we're injecting directories, fixes the absence of `/tmp` in our
    // images for example.
    for entry in entries {
        let header = entry.header();
        let entry_path = entry.path().unwrap().to_string_lossy().into_owned();
        if seen_paths.contains(&entry_path) {
            anyhow::bail!("Duplicate path: {}", entry_path);
        }
        seen_paths.insert(entry_path.clone());
        if let Some(exp) = expected.remove(entry_path.as_str()) {
            assert_eq!(header.entry_type(), exp.etype, "{}", entry_path);
            let is_old_object = format_version == 0;
            let mut expected_mode = exp.mode;
            if is_old_object && !entry_path.starts_with("sysroot/") {
                let fmtbits = match header.entry_type() {
                    tar::EntryType::Regular => libc::S_IFREG,
                    tar::EntryType::Directory => libc::S_IFDIR,
                    tar::EntryType::Symlink => 0,
                    o => panic!("Unexpected entry type {:?}", o),
                };
                expected_mode |= fmtbits;
            }
            assert_eq!(
                header.mode().unwrap(),
                expected_mode,
                "fmtver: {} type: {:?} path: {}",
                format_version,
                header.entry_type(),
                entry_path
            );
        }
    }

    assert!(
        expected.is_empty(),
        "Expected but not found:\n{:?}",
        expected
    );
    Ok(())
}

/// Validate basic structure of the tar export.
#[test]
fn test_tar_export_structure() -> Result<()> {
    use tar::EntryType::{Directory, Regular};

    let mut fixture = Fixture::new_v1()?;

    let src_tar = fixture.export_tar()?;
    let src_tar = std::io::BufReader::new(fixture.dir.open(src_tar)?);
    let mut src_tar = tar::Archive::new(src_tar);
    let mut entries = src_tar.entries()?;
    // The first entry should be the root directory.
    let first = entries.next().unwrap()?;
    let firstpath = first.path()?;
    assert_eq!(firstpath.to_str().unwrap(), "./");
    assert_eq!(first.header().mode()?, libc::S_IFDIR | 0o755);
    let next = entries.next().unwrap().unwrap();
    assert_eq!(next.path().unwrap().as_os_str(), "sysroot");

    // Validate format version 0
    let expected = [
        ("sysroot/config", Regular, 0o644),
        ("sysroot/ostree/repo", Directory, 0o755),
        ("sysroot/ostree/repo/extensions", Directory, 0o755),
        ("sysroot/ostree/repo/objects/00", Directory, 0o755),
        ("sysroot/ostree/repo/objects/23", Directory, 0o755),
        ("sysroot/ostree/repo/objects/77", Directory, 0o755),
        ("sysroot/ostree/repo/objects/bc", Directory, 0o755),
        ("sysroot/ostree/repo/objects/ff", Directory, 0o755),
        ("sysroot/ostree/repo/refs", Directory, 0o755),
        ("sysroot/ostree/repo/refs", Directory, 0o755),
        ("sysroot/ostree/repo/refs/heads", Directory, 0o755),
        ("sysroot/ostree/repo/refs/mirrors", Directory, 0o755),
        ("sysroot/ostree/repo/refs/remotes", Directory, 0o755),
        ("sysroot/ostree/repo/state", Directory, 0o755),
        ("sysroot/ostree/repo/tmp", Directory, 0o755),
        ("sysroot/ostree/repo/tmp/cache", Directory, 0o755),
        ("sysroot/ostree/repo/xattrs", Directory, 0o755),
        ("sysroot/ostree/repo/xattrs/d67db507c5a6e7bfd078f0f3ded0a5669479a902e812931fc65c6f5e01831ef5", Regular, 0o644),
        ("usr", Directory, 0o755),
    ];
    validate_tar_expected(
        fixture.format_version,
        entries,
        expected.iter().map(Into::into),
    )?;

    // Validate format version 1
    fixture.format_version = 1;
    let src_tar = fixture.export_tar()?;
    let src_tar = std::io::BufReader::new(fixture.dir.open(src_tar)?);
    let mut src_tar = tar::Archive::new(src_tar);
    let expected = [
        ("sysroot/ostree/repo", Directory, 0o755),
        ("sysroot/ostree/repo/config", Regular, 0o644),
        ("sysroot/ostree/repo/extensions", Directory, 0o755),
        ("sysroot/ostree/repo/objects/00", Directory, 0o755),
        ("sysroot/ostree/repo/objects/23", Directory, 0o755),
        ("sysroot/ostree/repo/objects/77", Directory, 0o755),
        ("sysroot/ostree/repo/objects/bc", Directory, 0o755),
        ("sysroot/ostree/repo/objects/ff", Directory, 0o755),
        ("sysroot/ostree/repo/refs", Directory, 0o755),
        ("sysroot/ostree/repo/refs", Directory, 0o755),
        ("sysroot/ostree/repo/refs/heads", Directory, 0o755),
        ("sysroot/ostree/repo/refs/mirrors", Directory, 0o755),
        ("sysroot/ostree/repo/refs/remotes", Directory, 0o755),
        ("sysroot/ostree/repo/state", Directory, 0o755),
        ("sysroot/ostree/repo/tmp", Directory, 0o755),
        ("sysroot/ostree/repo/tmp/cache", Directory, 0o755),
        ("usr", Directory, 0o755),
    ];
    validate_tar_expected(
        fixture.format_version,
        src_tar.entries()?,
        expected.iter().map(Into::into),
    )?;

    Ok(())
}

#[tokio::test]
async fn test_tar_import_export() -> Result<()> {
    let fixture = Fixture::new_v1()?;
    let p = fixture.export_tar()?;
    let src_tar = tokio::fs::File::from_std(fixture.dir.open(p)?.into_std());

    let imported_commit: String =
        ostree_ext::tar::import_tar(fixture.destrepo(), src_tar, None).await?;
    let (commitdata, _) = fixture.destrepo().load_commit(&imported_commit)?;
    assert_eq!(
        CONTENTS_CHECKSUM_V0,
        ostree::commit_get_content_checksum(&commitdata)
            .unwrap()
            .as_str()
    );
    bash_in!(
        &fixture.dir,
        r#"
         ostree --repo=dest/repo ls -R ${imported_commit} >/dev/null
         val=$(ostree --repo=dest/repo show --print-detached-metadata-key=my-detached-key ${imported_commit})
         test "${val}" = "'my-detached-value'"
        "#,
        imported_commit = imported_commit.as_str()
    )?;

    let (root, _) = fixture
        .destrepo()
        .read_commit(&imported_commit, gio::NONE_CANCELLABLE)?;
    let kdir = ostree_ext::bootabletree::find_kernel_dir(&root, gio::NONE_CANCELLABLE)?;
    let kdir = kdir.unwrap();
    assert_eq!(
        kdir.basename().unwrap().to_str().unwrap(),
        "5.10.18-200.x86_64"
    );

    Ok(())
}

#[tokio::test]
async fn test_tar_write() -> Result<()> {
    let fixture = Fixture::new_v1()?;
    // Test translating /etc to /usr/etc
    fixture.dir.create_dir_all("tmproot/etc")?;
    let tmproot = &fixture.dir.open_dir("tmproot")?;
    let tmpetc = tmproot.open_dir("etc")?;
    tmpetc.write("someconfig.conf", b"some config")?;
    tmproot.create_dir_all("var/log")?;
    let tmpvarlog = tmproot.open_dir("var/log")?;
    tmpvarlog.write("foo.log", "foolog")?;
    tmpvarlog.write("bar.log", "barlog")?;
    tmproot.create_dir("boot")?;
    let tmptar = "testlayer.tar";
    bash_in!(fixture.dir, "tar cf ${tmptar} -C tmproot .", tmptar)?;
    let src = fixture.dir.open(tmptar)?;
    fixture.dir.remove_file(tmptar)?;
    let src = tokio::fs::File::from_std(src.into_std());
    let r = ostree_ext::tar::write_tar(fixture.destrepo(), src, "layer", None).await?;
    bash_in!(
        &fixture.dir,
        "ostree --repo=dest/repo ls ${layer_commit} /usr/etc/someconfig.conf >/dev/null",
        layer_commit = r.commit.as_str()
    )?;
    assert_eq!(r.filtered.len(), 2);
    assert_eq!(*r.filtered.get("var").unwrap(), 4);
    assert_eq!(*r.filtered.get("boot").unwrap(), 1);

    Ok(())
}

#[tokio::test]
async fn test_tar_write_tar_layer() -> Result<()> {
    let fixture = Fixture::new_v1()?;
    let uncompressed_tar = tokio::io::BufReader::new(
        async_compression::tokio::bufread::GzipDecoder::new(EXAMPLE_TAR_LAYER),
    );
    ostree_ext::tar::write_tar(&fixture.destrepo(), uncompressed_tar, "test", None).await?;
    Ok(())
}

fn skopeo_inspect(imgref: &str) -> Result<String> {
    let out = Command::new("skopeo")
        .args(&["inspect", imgref])
        .stdout(std::process::Stdio::piped())
        .output()?;
    Ok(String::from_utf8(out.stdout)?)
}

fn skopeo_inspect_config(imgref: &str) -> Result<oci_spec::image::ImageConfiguration> {
    let out = Command::new("skopeo")
        .args(&["inspect", "--config", imgref])
        .stdout(std::process::Stdio::piped())
        .output()?;
    Ok(serde_json::from_slice(&out.stdout)?)
}

async fn impl_test_container_import_export(chunked: bool) -> Result<()> {
    let fixture = Fixture::new_v1()?;
    let testrev = fixture
        .srcrepo()
        .require_rev(fixture.testref())
        .context("Failed to resolve ref")?;

    let srcoci_path = &fixture.path.join("oci");
    let srcoci_imgref = ImageReference {
        transport: Transport::OciDir,
        name: srcoci_path.as_str().to_string(),
    };
    let config = Config {
        labels: Some(
            [("foo", "bar"), ("test", "value")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        ),
        ..Default::default()
    };
    // If chunking is requested, compute object ownership and size mappings
    let contentmeta = chunked
        .then(|| {
            let meta = fixture.get_object_meta().context("Computing object meta")?;
            ObjectMetaSized::compute_sizes(fixture.srcrepo(), meta).context("Computing sizes")
        })
        .transpose()?;
    let opts = ExportOpts {
        copy_meta_keys: vec!["buildsys.checksum".to_string()],
        ..Default::default()
    };
    let digest = ostree_ext::container::encapsulate(
        fixture.srcrepo(),
        fixture.testref(),
        &config,
        Some(opts),
        contentmeta,
        &srcoci_imgref,
    )
    .await
    .context("exporting")?;
    assert!(srcoci_path.exists());

    let inspect = skopeo_inspect(&srcoci_imgref.to_string())?;
    assert!(inspect.contains(r#""version": "42.0""#));
    assert!(inspect.contains(r#""foo": "bar""#));
    assert!(inspect.contains(r#""test": "value""#));
    assert!(inspect.contains(
        r#""buildsys.checksum": "41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3""#
    ));
    let cfg = skopeo_inspect_config(&srcoci_imgref.to_string())?;
    // unwrap.  Unwrap.  UnWrap.  UNWRAP!!!!!!!
    assert_eq!(
        cfg.config()
            .as_ref()
            .unwrap()
            .cmd()
            .as_ref()
            .unwrap()
            .get(0)
            .as_ref()
            .unwrap()
            .as_str(),
        "/usr/bin/bash"
    );

    let n_chunks = if chunked { 7 } else { 1 };
    assert_eq!(cfg.rootfs().diff_ids().len(), n_chunks);
    assert_eq!(cfg.history().len(), n_chunks);

    // Verify exporting to ociarchive
    {
        let archivepath = &fixture.path.join("export.ociarchive");
        let ociarchive_dest = ImageReference {
            transport: Transport::OciArchive,
            name: archivepath.as_str().to_string(),
        };
        let _: String = ostree_ext::container::encapsulate(
            fixture.srcrepo(),
            fixture.testref(),
            &config,
            None,
            None,
            &ociarchive_dest,
        )
        .await
        .context("exporting to ociarchive")
        .unwrap();
        assert!(archivepath.is_file());
    }

    let srcoci_unverified = OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: srcoci_imgref.clone(),
    };

    let (_, pushed_digest) = ostree_ext::container::fetch_manifest(&srcoci_unverified).await?;
    assert_eq!(pushed_digest, digest);

    // No remote matching
    let srcoci_unknownremote = OstreeImageReference {
        sigverify: SignatureSource::OstreeRemote("unknownremote".to_string()),
        imgref: srcoci_imgref.clone(),
    };
    let r = ostree_ext::container::unencapsulate(fixture.destrepo(), &srcoci_unknownremote)
        .await
        .context("importing");
    assert_err_contains(r, r#"Remote "unknownremote" not found"#);

    // Test with a signature
    let opts = glib::VariantDict::new(None);
    opts.insert("gpg-verify", &true);
    opts.insert("custom-backend", &"ostree-rs-ext");
    fixture
        .destrepo()
        .remote_add("myremote", None, Some(&opts.end()), gio::NONE_CANCELLABLE)?;
    bash_in!(
        &fixture.dir,
        "ostree --repo=dest/repo remote gpg-import --stdin myremote < src/gpghome/key1.asc",
    )?;
    let srcoci_verified = OstreeImageReference {
        sigverify: SignatureSource::OstreeRemote("myremote".to_string()),
        imgref: srcoci_imgref.clone(),
    };
    let import = ostree_ext::container::unencapsulate(fixture.destrepo(), &srcoci_verified)
        .await
        .context("importing")?;
    assert_eq!(import.ostree_commit, testrev.as_str());

    let temp_unsigned = ImageReference {
        transport: Transport::OciDir,
        name: fixture.path.join("unsigned.ocidir").to_string(),
    };
    let _: String =
        ostree_ext::container::update_detached_metadata(&srcoci_imgref, &temp_unsigned, None)
            .await
            .unwrap();
    let temp_unsigned = OstreeImageReference {
        sigverify: SignatureSource::OstreeRemote("myremote".to_string()),
        imgref: temp_unsigned,
    };
    fixture.clear_destrepo()?;
    let r = ostree_ext::container::unencapsulate(fixture.destrepo(), &temp_unsigned).await;
    assert_err_contains(r, "Expected commitmeta object");

    // Test without signature verification
    // Create a new repo
    {
        let fixture = Fixture::new_v1()?;
        let import = ostree_ext::container::unencapsulate(fixture.destrepo(), &srcoci_unverified)
            .await
            .context("importing")?;
        assert_eq!(import.ostree_commit, testrev.as_str());
    }

    Ok(())
}

#[tokio::test]
async fn impl_test_container_chunked() -> Result<()> {
    let nlayers = 6u32;
    let mut fixture = Fixture::new_v1()?;

    let (imgref, expected_digest) = fixture.export_container().await.unwrap();
    let imgref = OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: imgref,
    };

    let mut imp =
        store::ImageImporter::new(fixture.destrepo(), &imgref, Default::default()).await?;
    let prep = match imp.prepare().await.context("Init prep derived")? {
        store::PrepareResult::AlreadyPresent(_) => panic!("should not be already imported"),
        store::PrepareResult::Ready(r) => r,
    };
    let digest = prep.manifest_digest.clone();
    assert!(prep.ostree_commit_layer.commit.is_none());
    assert_eq!(prep.ostree_layers.len(), nlayers as usize);
    assert_eq!(prep.layers.len(), 0);
    for layer in prep.layers.iter() {
        assert!(layer.commit.is_none());
    }
    assert_eq!(digest, expected_digest);
    let _import = imp.import(prep).await.context("Init pull derived").unwrap();

    assert_eq!(store::list_images(fixture.destrepo()).unwrap().len(), 1);

    const ADDITIONS: &str = indoc::indoc! { "
r usr/bin/bash bash-v0
"};
    fixture
        .update(FileDef::iter_from(ADDITIONS), std::iter::empty())
        .context("Failed to update")?;

    let expected_digest = fixture.export_container().await.unwrap().1;
    assert_ne!(digest, expected_digest);

    let mut imp =
        store::ImageImporter::new(fixture.destrepo(), &imgref, Default::default()).await?;
    let prep = match imp.prepare().await.context("Init prep derived")? {
        store::PrepareResult::AlreadyPresent(_) => panic!("should not be already imported"),
        store::PrepareResult::Ready(r) => r,
    };
    let to_fetch = prep.layers_to_fetch().collect::<Result<Vec<_>>>()?;
    assert_eq!(to_fetch.len(), 2);
    assert_eq!(expected_digest, prep.manifest_digest.as_str());
    assert!(prep.ostree_commit_layer.commit.is_none());
    assert_eq!(prep.ostree_layers.len(), nlayers as usize);
    let (first, second) = (to_fetch[0], to_fetch[1]);
    assert_eq!(first.1, "bash");
    assert!(first.0.commit.is_none());
    assert!(second.1.starts_with("ostree export of commit"));
    assert!(second.0.commit.is_none());

    let _import = imp.import(prep).await.unwrap();

    assert_eq!(store::list_images(fixture.destrepo()).unwrap().len(), 1);

    let n_removed = store::gc_image_layers(&fixture.destrepo())?;
    assert_eq!(n_removed, 2);
    fixture
        .destrepo()
        .prune(ostree::RepoPruneFlags::REFS_ONLY, 0, gio::NONE_CANCELLABLE)?;

    // Build a derived image
    let derived_path = &fixture.path.join("derived.oci");
    let srcpath = imgref.imgref.name.as_str();
    oci_clone(srcpath, derived_path).await.unwrap();
    let temproot = &fixture.path.join("temproot");
    || -> Result<_> {
        std::fs::create_dir(temproot)?;
        let temprootd = Dir::open_ambient_dir(temproot, cap_std::ambient_authority())?;
        let mut db = DirBuilder::new();
        db.mode(0o755);
        db.recursive(true);
        temprootd.create_dir_with("usr/bin", &db)?;
        temprootd.write("usr/bin/newderivedfile", "newderivedfile v0")?;
        temprootd.write("usr/bin/newderivedfile3", "newderivedfile3 v0")?;
        Ok(())
    }()
    .context("generating temp content")?;
    ostree_ext::integrationtest::generate_derived_oci(derived_path, temproot)?;

    let derived_imgref = OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: ImageReference {
            transport: Transport::OciDir,
            name: derived_path.to_string(),
        },
    };
    let mut imp =
        store::ImageImporter::new(fixture.destrepo(), &derived_imgref, Default::default()).await?;
    let prep = match imp.prepare().await.unwrap() {
        store::PrepareResult::AlreadyPresent(_) => panic!("should not be already imported"),
        store::PrepareResult::Ready(r) => r,
    };
    let to_fetch = prep.layers_to_fetch().collect::<Result<Vec<_>>>()?;
    assert_eq!(to_fetch.len(), 1);
    assert!(prep.ostree_commit_layer.commit.is_some());
    assert_eq!(prep.ostree_layers.len(), nlayers as usize);

    let _import = imp.import(prep).await.unwrap();
    assert_eq!(store::list_images(fixture.destrepo()).unwrap().len(), 2);

    // Should only be new layers
    let n_removed = store::gc_image_layers(&fixture.destrepo())?;
    assert_eq!(n_removed, 0);
    store::remove_images(fixture.destrepo(), [&imgref.imgref]).unwrap();
    assert_eq!(store::list_images(fixture.destrepo()).unwrap().len(), 1);
    // Still no removed layers after removing the base image
    let n_removed = store::gc_image_layers(&fixture.destrepo())?;
    assert_eq!(n_removed, 0);
    store::remove_images(fixture.destrepo(), [&derived_imgref.imgref]).unwrap();
    assert_eq!(store::list_images(fixture.destrepo()).unwrap().len(), 0);
    let n_removed = store::gc_image_layers(&fixture.destrepo())?;
    assert_eq!(n_removed, 8);

    // Repo should be clean now
    assert_eq!(
        fixture
            .destrepo()
            .list_refs(None, gio::NONE_CANCELLABLE)
            .unwrap()
            .len(),
        0
    );

    Ok(())
}

/// Copy an OCI directory.
async fn oci_clone(src: impl AsRef<Utf8Path>, dest: impl AsRef<Utf8Path>) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();
    // For now we just fork off `cp` and rely on reflinks, but we could and should
    // explicitly hardlink blobs/sha256 e.g.
    let cmd = tokio::process::Command::new("cp")
        .args(&["-a", "--reflink=auto"])
        .args(&[src, dest])
        .status()
        .await?;
    if !cmd.success() {
        anyhow::bail!("cp failed");
    }
    Ok(())
}

#[tokio::test]
async fn test_container_import_export() -> Result<()> {
    impl_test_container_import_export(false).await.unwrap();
    impl_test_container_import_export(true).await.unwrap();
    Ok(())
}

/// But layers work via the container::write module.
#[tokio::test]
async fn test_container_write_derive() -> Result<()> {
    let fixture = Fixture::new_v1()?;
    let base_oci_path = &fixture.path.join("exampleos.oci");
    let _digest = ostree_ext::container::encapsulate(
        fixture.srcrepo(),
        fixture.testref(),
        &Config {
            cmd: Some(vec!["/bin/bash".to_string()]),
            ..Default::default()
        },
        None,
        None,
        &ImageReference {
            transport: Transport::OciDir,
            name: base_oci_path.to_string(),
        },
    )
    .await
    .context("exporting")?;
    assert!(base_oci_path.exists());

    // Build the derived images
    let derived_path = &fixture.path.join("derived.oci");
    oci_clone(base_oci_path, derived_path).await?;
    let temproot = &fixture.path.join("temproot");
    std::fs::create_dir_all(&temproot.join("usr/bin"))?;
    std::fs::write(temproot.join("usr/bin/newderivedfile"), "newderivedfile v0")?;
    std::fs::write(
        temproot.join("usr/bin/newderivedfile3"),
        "newderivedfile3 v0",
    )?;
    ostree_ext::integrationtest::generate_derived_oci(derived_path, temproot)?;
    // And v2
    let derived2_path = &fixture.path.join("derived2.oci");
    oci_clone(base_oci_path, derived2_path).await?;
    std::fs::remove_dir_all(temproot)?;
    std::fs::create_dir_all(&temproot.join("usr/bin"))?;
    std::fs::write(temproot.join("usr/bin/newderivedfile"), "newderivedfile v1")?;
    std::fs::write(
        temproot.join("usr/bin/newderivedfile2"),
        "newderivedfile2 v0",
    )?;
    ostree_ext::integrationtest::generate_derived_oci(derived2_path, temproot)?;

    let derived_ref = OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: ImageReference {
            transport: Transport::OciDir,
            name: derived_path.to_string(),
        },
    };
    // There shouldn't be any container images stored yet.
    let images = store::list_images(fixture.destrepo())?;
    assert!(images.is_empty());

    // Verify importing a derived image fails
    let r = ostree_ext::container::unencapsulate(fixture.destrepo(), &derived_ref).await;
    assert_err_contains(r, "Image has 1 non-ostree layers");

    // Pull a derived image - two layers, new base plus one layer.
    let mut imp =
        store::ImageImporter::new(fixture.destrepo(), &derived_ref, Default::default()).await?;
    let prep = match imp.prepare().await.context("Init prep derived")? {
        store::PrepareResult::AlreadyPresent(_) => panic!("should not be already imported"),
        store::PrepareResult::Ready(r) => r,
    };
    let expected_digest = prep.manifest_digest.clone();
    assert!(prep.ostree_commit_layer.commit.is_none());
    assert_eq!(prep.layers.len(), 1);
    for layer in prep.layers.iter() {
        assert!(layer.commit.is_none());
    }
    let import = imp.import(prep).await.context("Init pull derived")?;
    // We should have exactly one image stored.
    let images = store::list_images(fixture.destrepo())?;
    assert_eq!(images.len(), 1);
    assert_eq!(images[0], derived_ref.imgref.to_string());

    let imported_commit = &fixture
        .destrepo()
        .load_commit(import.merge_commit.as_str())?
        .0;
    let digest = store::manifest_digest_from_commit(imported_commit)?;
    assert!(digest.starts_with("sha256:"));
    assert_eq!(digest, expected_digest);

    let commit_meta = &imported_commit.child_value(0);
    let commit_meta = glib::VariantDict::new(Some(commit_meta));
    let config = commit_meta
        .lookup::<String>("ostree.container.image-config")?
        .unwrap();
    let config: oci_spec::image::ImageConfiguration = serde_json::from_str(&config)?;
    assert_eq!(config.os(), &oci_spec::image::Os::Linux);

    // Parse the commit and verify we pulled the derived content.
    bash_in!(
        &fixture.dir,
        "ostree --repo=dest/repo ls ${r} /usr/bin/newderivedfile >/dev/null",
        r = import.merge_commit.as_str()
    )?;

    // Import again, but there should be no changes.
    let mut imp =
        store::ImageImporter::new(fixture.destrepo(), &derived_ref, Default::default()).await?;
    let already_present = match imp.prepare().await? {
        store::PrepareResult::AlreadyPresent(c) => c,
        store::PrepareResult::Ready(_) => {
            panic!("Should have already imported {}", &derived_ref)
        }
    };
    assert_eq!(import.merge_commit, already_present.merge_commit);

    // Test upgrades; replace the oci-archive with new content.
    std::fs::remove_dir_all(derived_path)?;
    std::fs::rename(derived2_path, derived_path)?;
    let mut imp =
        store::ImageImporter::new(fixture.destrepo(), &derived_ref, Default::default()).await?;
    let prep = match imp.prepare().await? {
        store::PrepareResult::AlreadyPresent(_) => panic!("should not be already imported"),
        store::PrepareResult::Ready(r) => r,
    };
    // We *should* already have the base layer.
    assert!(prep.ostree_commit_layer.commit.is_some());
    // One new layer
    assert_eq!(prep.layers.len(), 1);
    for layer in prep.layers.iter() {
        assert!(layer.commit.is_none());
    }
    let import = imp.import(prep).await?;
    // New commit.
    assert_ne!(import.merge_commit, already_present.merge_commit);
    // We should still have exactly one image stored.
    let images = store::list_images(fixture.destrepo())?;
    assert_eq!(images[0], derived_ref.imgref.to_string());
    assert_eq!(images.len(), 1);

    // Verify we have the new file and *not* the old one
    bash_in!(
        &fixture.dir,
        r#"set -x;
         ostree --repo=dest/repo ls ${r} /usr/bin/newderivedfile2 >/dev/null
         test "$(ostree --repo=dest/repo cat ${r} /usr/bin/newderivedfile)" = "newderivedfile v1"
         if ostree --repo=dest/repo ls ${r} /usr/bin/newderivedfile3 2>/dev/null; then
           echo oops; exit 1
         fi
        "#,
        r = import.merge_commit.as_str()
    )?;

    // And there should be no changes on upgrade again.
    let mut imp =
        store::ImageImporter::new(fixture.destrepo(), &derived_ref, Default::default()).await?;
    let already_present = match imp.prepare().await? {
        store::PrepareResult::AlreadyPresent(c) => c,
        store::PrepareResult::Ready(_) => {
            panic!("Should have already imported {}", &derived_ref)
        }
    };
    assert_eq!(import.merge_commit, already_present.merge_commit);

    // Create a new repo, and copy to it
    let destrepo2 = ostree::Repo::create_at(
        ostree::AT_FDCWD,
        fixture.path.join("destrepo2").as_str(),
        ostree::RepoMode::Archive,
        None,
        gio::NONE_CANCELLABLE,
    )?;
    store::copy(fixture.destrepo(), &destrepo2, &derived_ref).await?;

    let images = store::list_images(&destrepo2)?;
    assert_eq!(images.len(), 1);
    assert_eq!(images[0], derived_ref.imgref.to_string());

    Ok(())
}

#[ignore]
#[tokio::test]
// Verify that we can push and pull to a registry, not just oci-archive:.
// This requires a registry set up externally right now.  One can run a HTTP registry via e.g.
// `podman run --rm -ti -p 5000:5000 --name registry docker.io/library/registry:2`
// but that doesn't speak HTTPS and adding that is complex.
// A simple option is setting up e.g. quay.io/$myuser/exampleos and then do:
// Then you can run this test via `env TEST_REGISTRY=quay.io/$myuser cargo test -- --ignored`.
async fn test_container_import_export_registry() -> Result<()> {
    let tr = &*TEST_REGISTRY;
    let fixture = Fixture::new_v1()?;
    let testref = fixture.testref();
    let testrev = fixture
        .srcrepo()
        .require_rev(testref)
        .context("Failed to resolve ref")?;
    let src_imgref = ImageReference {
        transport: Transport::Registry,
        name: format!("{}/exampleos", tr),
    };
    let config = Config {
        cmd: Some(vec!["/bin/bash".to_string()]),
        ..Default::default()
    };
    let digest = ostree_ext::container::encapsulate(
        fixture.srcrepo(),
        testref,
        &config,
        None,
        None,
        &src_imgref,
    )
    .await
    .context("exporting to registry")?;
    let mut digested_imgref = src_imgref.clone();
    digested_imgref.name = format!("{}@{}", src_imgref.name, digest);

    let import_ref = OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: digested_imgref,
    };
    let import = ostree_ext::container::unencapsulate(fixture.destrepo(), &import_ref)
        .await
        .context("importing")?;
    assert_eq!(import.ostree_commit, testrev.as_str());
    Ok(())
}

#[test]
fn test_diff() -> Result<()> {
    let mut fixture = Fixture::new_v1()?;
    const ADDITIONS: &str = indoc::indoc! { "
r /usr/bin/newbin some-new-binary
d /usr/share
"};
    fixture
        .update(
            FileDef::iter_from(ADDITIONS),
            IntoIterator::into_iter([Cow::Borrowed("/usr/bin/bash".into())]),
        )
        .context("Failed to update")?;
    let from = &format!("{}^", fixture.testref());
    let repo = fixture.srcrepo();
    let subdir: Option<&str> = None;
    let diff = ostree_ext::diff::diff(repo, from, fixture.testref(), subdir)?;
    assert!(diff.subdir.is_none());
    assert_eq!(diff.added_dirs.len(), 1);
    assert_eq!(diff.added_dirs.iter().next().unwrap(), "/usr/share");
    assert_eq!(diff.added_files.len(), 1);
    assert_eq!(diff.added_files.iter().next().unwrap(), "/usr/bin/newbin");
    assert_eq!(diff.removed_files.len(), 1);
    assert_eq!(diff.removed_files.iter().next().unwrap(), "/usr/bin/bash");
    let diff = ostree_ext::diff::diff(repo, from, fixture.testref(), Some("/usr"))?;
    assert_eq!(diff.subdir.as_ref().unwrap(), "/usr");
    assert_eq!(diff.added_dirs.len(), 1);
    assert_eq!(diff.added_dirs.iter().next().unwrap(), "/share");
    assert_eq!(diff.added_files.len(), 1);
    assert_eq!(diff.added_files.iter().next().unwrap(), "/bin/newbin");
    assert_eq!(diff.removed_files.len(), 1);
    assert_eq!(diff.removed_files.iter().next().unwrap(), "/bin/bash");
    Ok(())
}
