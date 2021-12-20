use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use indoc::indoc;
use once_cell::sync::Lazy;
use ostree_ext::container::store::PrepareResult;
use ostree_ext::container::{
    Config, ImageReference, OstreeImageReference, SignatureSource, Transport,
};
use ostree_ext::tar::TarImportOptions;
use ostree_ext::{gio, glib};
use sh_inline::bash;
use std::convert::TryInto;
use std::{io::Write, process::Command};

const OSTREE_GPG_HOME: &[u8] = include_bytes!("fixtures/ostree-gpg-test-home.tar.gz");
const TEST_GPG_KEYID_1: &str = "7FCA23D8472CDAFA";
#[allow(dead_code)]
const TEST_GPG_KEYFPR_1: &str = "5E65DE75AB1C501862D476347FCA23D8472CDAFA";
const EXAMPLEOS_V0: &[u8] = include_bytes!("fixtures/exampleos.tar.zst");
const EXAMPLEOS_V1: &[u8] = include_bytes!("fixtures/exampleos-v1.tar.zst");
const TESTREF: &str = "exampleos/x86_64/stable";
const EXAMPLEOS_CONTENT_CHECKSUM: &str =
    "0ef7461f9db15e1d8bd8921abf20694225fbaa4462cadf7deed8ea0e43162120";
const TEST_REGISTRY_DEFAULT: &str = "localhost:5000";

fn assert_err_contains<T>(r: Result<T>, s: impl AsRef<str>) {
    let s = s.as_ref();
    let msg = format!("{:#}", r.err().unwrap());
    if !msg.contains(s) {
        panic!(r#"Error message "{}" did not contain "{}""#, msg, s);
    }
}

static TEST_REGISTRY: Lazy<String> = Lazy::new(|| match std::env::var_os("TEST_REGISTRY") {
    Some(t) => t.to_str().unwrap().to_owned(),
    None => TEST_REGISTRY_DEFAULT.to_string(),
});

#[context("Generating test repo")]
fn generate_test_repo(dir: &Utf8Path) -> Result<Utf8PathBuf> {
    let src_tarpath = &dir.join("exampleos.tar.zst");
    std::fs::write(src_tarpath, EXAMPLEOS_V0)?;

    let gpghome = dir.join("gpghome");
    {
        let dec = flate2::read::GzDecoder::new(OSTREE_GPG_HOME);
        let mut a = tar::Archive::new(dec);
        a.unpack(&gpghome)?;
    };

    bash!(
        indoc! {"
        cd {dir}
        ostree --repo=repo init --mode=archive
        ostree --repo=repo commit -b {testref} --bootable --no-bindings --add-metadata-string=version=42.0 --gpg-homedir={gpghome} --gpg-sign={keyid} \
               --add-detached-metadata-string=my-detached-key=my-detached-value --tree=tar=exampleos.tar.zst
        ostree --repo=repo show {testref}
    "},
        testref = TESTREF,
        gpghome = gpghome.as_str(),
        keyid = TEST_GPG_KEYID_1,
        dir = dir.as_str()
    )?;
    std::fs::remove_file(src_tarpath)?;
    Ok(dir.join("repo"))
}

fn update_repo(repopath: &Utf8Path) -> Result<()> {
    let repotmp = &repopath.join("tmp");
    let srcpath = &repotmp.join("exampleos-v1.tar.zst");
    std::fs::write(srcpath, EXAMPLEOS_V1)?;
    let srcpath = srcpath.as_str();
    let repopath = repopath.as_str();
    let testref = TESTREF;
    bash!(
        "ostree --repo={repopath} commit -b {testref} --no-bindings --tree=tar={srcpath}",
        testref,
        repopath,
        srcpath
    )?;
    std::fs::remove_file(srcpath)?;
    Ok(())
}

#[context("Generating test tarball")]
fn initial_export(fixture: &Fixture) -> Result<Utf8PathBuf> {
    let cancellable = gio::NONE_CANCELLABLE;
    let (_, rev) = fixture.srcrepo.read_commit(TESTREF, cancellable)?;
    let (commitv, _) = fixture.srcrepo.load_commit(rev.as_str())?;
    assert_eq!(
        ostree::commit_get_content_checksum(&commitv)
            .unwrap()
            .as_str(),
        EXAMPLEOS_CONTENT_CHECKSUM
    );
    let destpath = fixture.path.join("exampleos-export.tar");
    let mut outf = std::io::BufWriter::new(std::fs::File::create(&destpath)?);
    let options = ostree_ext::tar::ExportOptions {
        format_version: fixture.format_version,
        ..Default::default()
    };
    ostree_ext::tar::export_commit(&fixture.srcrepo, rev.as_str(), &mut outf, Some(options))?;
    outf.flush()?;
    Ok(destpath)
}

struct Fixture {
    // Just holds a reference
    _tempdir: tempfile::TempDir,
    path: Utf8PathBuf,
    srcdir: Utf8PathBuf,
    srcrepo: ostree::Repo,
    destrepo: ostree::Repo,
    destrepo_path: Utf8PathBuf,

    format_version: u32,
}

impl Fixture {
    fn new() -> Result<Self> {
        let _tempdir = tempfile::tempdir_in("/var/tmp")?;
        let path: &Utf8Path = _tempdir.path().try_into().unwrap();
        let path = path.to_path_buf();

        let srcdir = path.join("src");
        std::fs::create_dir(&srcdir)?;
        let srcrepo_path = generate_test_repo(&srcdir)?;
        let srcrepo =
            ostree::Repo::open_at(libc::AT_FDCWD, srcrepo_path.as_str(), gio::NONE_CANCELLABLE)?;

        let destdir = &path.join("dest");
        std::fs::create_dir(destdir)?;
        let destrepo_path = destdir.join("repo");
        let destrepo = ostree::Repo::new_for_path(&destrepo_path);
        destrepo.create(ostree::RepoMode::BareUser, gio::NONE_CANCELLABLE)?;
        Ok(Self {
            _tempdir,
            path,
            srcdir,
            srcrepo,
            destrepo,
            destrepo_path,
            format_version: 0,
        })
    }
}

#[tokio::test]
async fn test_tar_import_empty() -> Result<()> {
    let fixture = Fixture::new()?;
    let r = ostree_ext::tar::import_tar(&fixture.destrepo, tokio::io::empty(), None).await;
    assert_err_contains(r, "Commit object not found");
    Ok(())
}

#[tokio::test]
async fn test_tar_export_reproducible() -> Result<()> {
    let fixture = Fixture::new()?;
    let (_, rev) = fixture
        .srcrepo
        .read_commit(TESTREF, gio::NONE_CANCELLABLE)?;
    let export1 = {
        let mut h = openssl::hash::Hasher::new(openssl::hash::MessageDigest::sha256())?;
        ostree_ext::tar::export_commit(&fixture.srcrepo, rev.as_str(), &mut h, None)?;
        h.finish()?
    };
    // Artificial delay to flush out mtimes (one second granularity baseline, plus another 100ms for good measure).
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let export2 = {
        let mut h = openssl::hash::Hasher::new(openssl::hash::MessageDigest::sha256())?;
        ostree_ext::tar::export_commit(&fixture.srcrepo, rev.as_str(), &mut h, None)?;
        h.finish()?
    };
    assert_eq!(*export1, *export2);
    Ok(())
}

#[tokio::test]
async fn test_tar_import_signed() -> Result<()> {
    let fixture = Fixture::new()?;
    let test_tar = &initial_export(&fixture)?;

    // Verify we fail with an unknown remote.
    let src_tar = tokio::fs::File::open(test_tar).await?;
    let r = ostree_ext::tar::import_tar(
        &fixture.destrepo,
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
        .destrepo
        .remote_add("myremote", None, Some(&opts.end()), gio::NONE_CANCELLABLE)?;
    let src_tar = tokio::fs::File::open(test_tar).await?;
    let r = ostree_ext::tar::import_tar(
        &fixture.destrepo,
        src_tar,
        Some(TarImportOptions {
            remote: Some("myremote".to_string()),
        }),
    )
    .await;
    assert_err_contains(r, r#"Can't check signature: public key not found"#);

    // And signed correctly
    bash!(
        "ostree --repo={repo} remote gpg-import --stdin myremote < {p}/gpghome/key1.asc",
        repo = fixture.destrepo_path.as_str(),
        p = fixture.srcdir.as_str()
    )?;
    let src_tar = tokio::fs::File::open(test_tar).await?;
    let imported = ostree_ext::tar::import_tar(
        &fixture.destrepo,
        src_tar,
        Some(TarImportOptions {
            remote: Some("myremote".to_string()),
        }),
    )
    .await?;
    let (commitdata, state) = fixture.destrepo.load_commit(&imported)?;
    assert_eq!(
        EXAMPLEOS_CONTENT_CHECKSUM,
        ostree::commit_get_content_checksum(&commitdata)
            .unwrap()
            .as_str()
    );
    assert_eq!(state, ostree::RepoCommitState::NORMAL);
    Ok(())
}

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
    t: tar::Entries<T>,
    expected: impl IntoIterator<Item = TarExpected>,
) -> Result<()> {
    let expected = expected.into_iter();
    let mut entries = t.map(|e| e.unwrap());
    // Verify we're injecting directories, fixes the absence of `/tmp` in our
    // images for example.
    for exp in expected {
        let mut found = false;
        while let Some(entry) = entries.next() {
            let header = entry.header();
            let entry_path = entry.path().unwrap();
            if exp.path == entry_path.as_os_str() {
                assert_eq!(header.entry_type(), exp.etype);
                assert_eq!(header.mode().unwrap(), exp.mode);
                found = true;
                break;
            }
        }
        if !found {
            anyhow::bail!("Failed to find entry: {}", exp.path);
        }
    }
    Ok(())
}

/// Validate basic structure of the tar export.
/// Right now just checks the first entry is `sysroot` with mode 0755.
#[test]
fn test_tar_export_structure() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let src_tar = initial_export(&fixture)?;
    let src_tar = std::io::BufReader::new(std::fs::File::open(&src_tar)?);
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
        ("sysroot/config", tar::EntryType::Regular, 0o644),
        ("usr", tar::EntryType::Directory, libc::S_IFDIR | 0o755),
    ];
    validate_tar_expected(entries, expected.iter().map(Into::into))?;

    // Validate format version 1
    fixture.format_version = 1;
    let src_tar = initial_export(&fixture)?;
    let src_tar = std::io::BufReader::new(std::fs::File::open(&src_tar)?);
    let mut src_tar = tar::Archive::new(src_tar);
    let expected = [
        ("sysroot/ostree/repo/config", tar::EntryType::Regular, 0o644),
        ("usr", tar::EntryType::Directory, libc::S_IFDIR | 0o755),
    ];
    validate_tar_expected(src_tar.entries()?, expected.iter().map(Into::into))?;

    Ok(())
}

#[tokio::test]
async fn test_tar_import_export() -> Result<()> {
    let fixture = Fixture::new()?;
    let src_tar = tokio::fs::File::open(&initial_export(&fixture)?).await?;

    let imported_commit: String =
        ostree_ext::tar::import_tar(&fixture.destrepo, src_tar, None).await?;
    let (commitdata, _) = fixture.destrepo.load_commit(&imported_commit)?;
    assert_eq!(
        EXAMPLEOS_CONTENT_CHECKSUM,
        ostree::commit_get_content_checksum(&commitdata)
            .unwrap()
            .as_str()
    );
    bash!(
        r#"
         ostree --repo={destrepodir} ls -R {imported_commit}
         val=$(ostree --repo={destrepodir} show --print-detached-metadata-key=my-detached-key {imported_commit})
         test "${{val}}" = "'my-detached-value'"
        "#,
        destrepodir = fixture.destrepo_path.as_str(),
        imported_commit = imported_commit.as_str()
    )?;
    Ok(())
}

#[tokio::test]
async fn test_tar_write() -> Result<()> {
    let fixture = Fixture::new()?;
    // Test translating /etc to /usr/etc
    let tmpetc = fixture.path.join("tmproot/etc");
    std::fs::create_dir_all(&tmpetc)?;
    std::fs::write(tmpetc.join("someconfig.conf"), b"")?;
    let tmproot = tmpetc.parent().unwrap();
    let tmpvarlib = &tmproot.join("var/lib");
    std::fs::create_dir_all(tmpvarlib)?;
    std::fs::write(tmpvarlib.join("foo.log"), "foolog")?;
    std::fs::write(tmpvarlib.join("bar.log"), "barlog")?;
    std::fs::create_dir_all(tmproot.join("boot"))?;
    let tmptar = fixture.path.join("testlayer.tar");
    bash!(
        "tar cf {tmptar} -C {tmproot} .",
        tmptar = tmptar.as_str(),
        tmproot = tmproot.as_str()
    )?;
    let src = tokio::fs::File::open(&tmptar).await?;
    let r = ostree_ext::tar::write_tar(&fixture.destrepo, src, "layer", None).await?;
    bash!(
        "ostree --repo={repo} ls {layer_commit} /usr/etc/someconfig.conf >/dev/null",
        repo = fixture.destrepo_path.as_str(),
        layer_commit = r.commit.as_str()
    )?;
    assert_eq!(r.filtered.len(), 2);
    assert_eq!(*r.filtered.get("var").unwrap(), 4);
    assert_eq!(*r.filtered.get("boot").unwrap(), 1);

    Ok(())
}

fn skopeo_inspect(imgref: &str) -> Result<String> {
    let out = Command::new("skopeo")
        .args(&["inspect", imgref])
        .stdout(std::process::Stdio::piped())
        .output()?;
    Ok(String::from_utf8(out.stdout)?)
}

#[tokio::test]
async fn test_container_import_export() -> Result<()> {
    let fixture = Fixture::new()?;
    let testrev = fixture
        .srcrepo
        .resolve_rev(TESTREF, false)
        .context("Failed to resolve ref")?
        .unwrap();

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
        cmd: Some(vec!["/bin/bash".to_string()]),
    };
    let digest = ostree_ext::container::encapsulate(
        &fixture.srcrepo,
        TESTREF,
        &config,
        None,
        &srcoci_imgref,
    )
    .await
    .context("exporting")?;
    assert!(srcoci_path.exists());

    let inspect = skopeo_inspect(&srcoci_imgref.to_string())?;
    assert!(inspect.contains(r#""version": "42.0""#));
    assert!(inspect.contains(r#""foo": "bar""#));
    assert!(inspect.contains(r#""test": "value""#));

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
    let r = ostree_ext::container::unencapsulate(&fixture.destrepo, &srcoci_unknownremote, None)
        .await
        .context("importing");
    assert_err_contains(r, r#"Remote "unknownremote" not found"#);

    // Test with a signature
    let opts = glib::VariantDict::new(None);
    opts.insert("gpg-verify", &true);
    opts.insert("custom-backend", &"ostree-rs-ext");
    fixture
        .destrepo
        .remote_add("myremote", None, Some(&opts.end()), gio::NONE_CANCELLABLE)?;
    bash!(
        "ostree --repo={repo} remote gpg-import --stdin myremote < {p}/gpghome/key1.asc",
        repo = fixture.destrepo_path.as_str(),
        p = fixture.srcdir.as_str()
    )?;

    // No remote matching
    let srcoci_verified = OstreeImageReference {
        sigverify: SignatureSource::OstreeRemote("myremote".to_string()),
        imgref: srcoci_imgref.clone(),
    };
    let import = ostree_ext::container::unencapsulate(&fixture.destrepo, &srcoci_verified, None)
        .await
        .context("importing")?;
    assert_eq!(import.ostree_commit, testrev.as_str());

    // Test without signature verification
    // Create a new repo
    {
        let fixture = Fixture::new()?;
        let import =
            ostree_ext::container::unencapsulate(&fixture.destrepo, &srcoci_unverified, None)
                .await
                .context("importing")?;
        assert_eq!(import.ostree_commit, testrev.as_str());
    }

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

/// But layers work via the container::write module.
#[tokio::test]
async fn test_container_write_derive() -> Result<()> {
    let fixture = Fixture::new()?;
    let base_oci_path = &fixture.path.join("exampleos.oci");
    let _digest = ostree_ext::container::encapsulate(
        &fixture.srcrepo,
        TESTREF,
        &Config {
            cmd: Some(vec!["/bin/bash".to_string()]),
            ..Default::default()
        },
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
    let images = ostree_ext::container::store::list_images(&fixture.destrepo)?;
    assert!(images.is_empty());

    // Verify importing a derive dimage fails
    let r = ostree_ext::container::unencapsulate(&fixture.destrepo, &derived_ref, None).await;
    assert_err_contains(r, "Expected 1 layer, found 2");

    // Pull a derived image - two layers, new base plus one layer.
    let mut imp = ostree_ext::container::store::LayeredImageImporter::new(
        &fixture.destrepo,
        &derived_ref,
        Default::default(),
    )
    .await?;
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(_) => panic!("should not be already imported"),
        PrepareResult::Ready(r) => r,
    };
    let expected_digest = prep.manifest_digest.clone();
    assert!(prep.base_layer.commit.is_none());
    assert_eq!(prep.layers.len(), 1);
    for layer in prep.layers.iter() {
        assert!(layer.commit.is_none());
    }
    let import = imp.import(prep).await?;
    // We should have exactly one image stored.
    let images = ostree_ext::container::store::list_images(&fixture.destrepo)?;
    assert_eq!(images.len(), 1);
    assert_eq!(images[0], derived_ref.imgref.to_string());

    let imported_commit = &fixture
        .destrepo
        .load_commit(import.merge_commit.as_str())?
        .0;
    let digest = ostree_ext::container::store::manifest_digest_from_commit(imported_commit)?;
    assert!(digest.starts_with("sha256:"));
    assert_eq!(digest, expected_digest);

    #[cfg(feature = "proxy_v0_2_3")]
    {
        let commit_meta = &imported_commit.child_value(0);
        let proxy = containers_image_proxy::ImageProxy::new().await?;
        let commit_meta = glib::VariantDict::new(Some(commit_meta));
        let config = commit_meta
            .lookup::<String>("ostree.container.image-config")?
            .unwrap();
        let config: oci_spec::image::ImageConfiguration = serde_json::from_str(&config)?;
        assert_eq!(config.os(), &oci_spec::image::Os::Linux);
    }

    // Parse the commit and verify we pulled the derived content.
    bash!(
        "ostree --repo={repo} ls {r} /usr/bin/newderivedfile",
        repo = fixture.destrepo_path.as_str(),
        r = import.merge_commit.as_str()
    )?;

    // Import again, but there should be no changes.
    let mut imp = ostree_ext::container::store::LayeredImageImporter::new(
        &fixture.destrepo,
        &derived_ref,
        Default::default(),
    )
    .await?;
    let already_present = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => c,
        PrepareResult::Ready(_) => {
            panic!("Should have already imported {}", &derived_ref)
        }
    };
    assert_eq!(import.merge_commit, already_present.merge_commit);

    // Test upgrades; replace the oci-archive with new content.
    std::fs::remove_dir_all(derived_path)?;
    std::fs::rename(derived2_path, derived_path)?;
    let mut imp = ostree_ext::container::store::LayeredImageImporter::new(
        &fixture.destrepo,
        &derived_ref,
        Default::default(),
    )
    .await?;
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(_) => panic!("should not be already imported"),
        PrepareResult::Ready(r) => r,
    };
    // We *should* already have the base layer.
    assert!(prep.base_layer.commit.is_some());
    // One new layer
    assert_eq!(prep.layers.len(), 1);
    for layer in prep.layers.iter() {
        assert!(layer.commit.is_none());
    }
    let import = imp.import(prep).await?;
    // New commit.
    assert_ne!(import.merge_commit, already_present.merge_commit);
    // We should still have exactly one image stored.
    let images = ostree_ext::container::store::list_images(&fixture.destrepo)?;
    assert_eq!(images[0], derived_ref.imgref.to_string());
    assert_eq!(images.len(), 1);

    // Verify we have the new file and *not* the old one
    bash!(
        r#"set -x;
         ostree --repo={repo} ls {r} /usr/bin/newderivedfile2 >/dev/null
         test "$(ostree --repo={repo} cat {r} /usr/bin/newderivedfile)" = "newderivedfile v1"
         if ostree --repo={repo} ls {r} /usr/bin/newderivedfile3 2>/dev/null; then
           echo oops; exit 1
         fi
        "#,
        repo = fixture.destrepo_path.as_str(),
        r = import.merge_commit.as_str()
    )?;

    // And there should be no changes on upgrade again.
    let mut imp = ostree_ext::container::store::LayeredImageImporter::new(
        &fixture.destrepo,
        &derived_ref,
        Default::default(),
    )
    .await?;
    let already_present = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => c,
        PrepareResult::Ready(_) => {
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
    ostree_ext::container::store::copy(&fixture.destrepo, &destrepo2, &derived_ref).await?;

    let images = ostree_ext::container::store::list_images(&destrepo2)?;
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
    let fixture = Fixture::new()?;
    let testrev = fixture
        .srcrepo
        .resolve_rev(TESTREF, false)
        .context("Failed to resolve ref")?
        .unwrap();
    let src_imgref = ImageReference {
        transport: Transport::Registry,
        name: format!("{}/exampleos", tr),
    };
    let config = Config {
        cmd: Some(vec!["/bin/bash".to_string()]),
        ..Default::default()
    };
    let digest =
        ostree_ext::container::encapsulate(&fixture.srcrepo, TESTREF, &config, None, &src_imgref)
            .await
            .context("exporting to registry")?;
    let mut digested_imgref = src_imgref.clone();
    digested_imgref.name = format!("{}@{}", src_imgref.name, digest);

    let import_ref = OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: digested_imgref,
    };
    let import = ostree_ext::container::unencapsulate(&fixture.destrepo, &import_ref, None)
        .await
        .context("importing")?;
    assert_eq!(import.ostree_commit, testrev.as_str());
    Ok(())
}

#[test]
fn test_diff() -> Result<()> {
    let cancellable = gio::NONE_CANCELLABLE;
    let tempdir = tempfile::tempdir()?;
    let tempdir = Utf8Path::from_path(tempdir.path()).unwrap();
    let repopath = &generate_test_repo(tempdir)?;
    update_repo(repopath)?;
    let from = &format!("{}^", TESTREF);
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, repopath.as_str(), cancellable)?;
    let subdir: Option<&str> = None;
    let diff = ostree_ext::diff::diff(repo, from, TESTREF, subdir)?;
    assert!(diff.subdir.is_none());
    assert_eq!(diff.added_dirs.len(), 1);
    assert_eq!(diff.added_dirs.iter().next().unwrap(), "/usr/share");
    assert_eq!(diff.added_files.len(), 1);
    assert_eq!(diff.added_files.iter().next().unwrap(), "/usr/bin/newbin");
    assert_eq!(diff.removed_files.len(), 1);
    assert_eq!(diff.removed_files.iter().next().unwrap(), "/usr/bin/foo");
    let diff = ostree_ext::diff::diff(repo, from, TESTREF, Some("/usr"))?;
    assert_eq!(diff.subdir.as_ref().unwrap(), "/usr");
    assert_eq!(diff.added_dirs.len(), 1);
    assert_eq!(diff.added_dirs.iter().next().unwrap(), "/share");
    assert_eq!(diff.added_files.len(), 1);
    assert_eq!(diff.added_files.iter().next().unwrap(), "/bin/newbin");
    assert_eq!(diff.removed_files.len(), 1);
    assert_eq!(diff.removed_files.iter().next().unwrap(), "/bin/foo");
    Ok(())
}
