use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use indoc::indoc;
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

/// Image that contains a base exported layer, then a `podman build` of an added file on top.
const EXAMPLEOS_DERIVED_OCI: &[u8] = include_bytes!("fixtures/exampleos-derive.ociarchive");

fn assert_err_contains<T>(r: Result<T>, s: impl AsRef<str>) {
    let s = s.as_ref();
    let msg = format!("{:#}", r.err().unwrap());
    if !msg.contains(s) {
        panic!(r#"Error message "{}" did not contain "{}""#, msg, s);
    }
}

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
        ostree --repo=repo commit -b {testref} --bootable --add-metadata-string=version=42.0 --gpg-homedir={gpghome} --gpg-sign={keyid} \
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
        "ostree --repo={repopath} commit -b {testref} --tree=tar={srcpath}",
        testref,
        repopath,
        srcpath
    )?;
    std::fs::remove_file(srcpath)?;
    Ok(())
}

#[context("Generating test tarball")]
fn generate_test_tarball(dir: &Utf8Path) -> Result<Utf8PathBuf> {
    let cancellable = gio::NONE_CANCELLABLE;
    let repopath = generate_test_repo(dir)?;
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, repopath.as_str(), cancellable)?;
    let (_, rev) = repo.read_commit(TESTREF, cancellable)?;
    let (commitv, _) = repo.load_commit(rev.as_str())?;
    assert_eq!(
        ostree::commit_get_content_checksum(&commitv)
            .unwrap()
            .as_str(),
        EXAMPLEOS_CONTENT_CHECKSUM
    );
    let destpath = dir.join("exampleos-export.tar");
    let mut outf = std::io::BufWriter::new(std::fs::File::create(&destpath)?);
    ostree_ext::tar::export_commit(repo, rev.as_str(), &mut outf)?;
    outf.flush()?;
    Ok(destpath)
}

struct Fixture {
    // Just holds a reference
    _tempdir: tempfile::TempDir,
    path: Utf8PathBuf,
    destrepo: ostree::Repo,
    destrepo_path: Utf8PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let _tempdir = tempfile::tempdir_in("/var/tmp")?;
        let path: &Utf8Path = _tempdir.path().try_into().unwrap();
        let path = path.to_path_buf();
        let destdir = &path.join("dest");
        std::fs::create_dir(destdir)?;
        let destrepo_path = destdir.join("repo");
        let destrepo = ostree::Repo::new_for_path(&destrepo_path);
        destrepo.create(ostree::RepoMode::BareUser, gio::NONE_CANCELLABLE)?;
        Ok(Self {
            _tempdir,
            path,
            destrepo,
            destrepo_path,
        })
    }
}

#[tokio::test]
async fn test_tar_import_empty() -> Result<()> {
    let fixture = Fixture::new()?;
    let destrepo = ostree::Repo::new_for_path(&fixture.destrepo_path);
    destrepo.open(gio::NONE_CANCELLABLE)?;
    let r = ostree_ext::tar::import_tar(&destrepo, tokio::io::empty(), None).await;
    assert!(r.is_err());
    Ok(())
}

#[tokio::test]
async fn test_tar_import_signed() -> Result<()> {
    let fixture = Fixture::new()?;
    let srcdir = &fixture.path.join("src");
    std::fs::create_dir(srcdir)?;

    let test_tar = &generate_test_tarball(srcdir)?;

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
        p = srcdir.as_str()
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
    let (commitdata, _) = fixture.destrepo.load_commit(&imported)?;
    assert_eq!(
        EXAMPLEOS_CONTENT_CHECKSUM,
        ostree::commit_get_content_checksum(&commitdata)
            .unwrap()
            .as_str()
    );
    Ok(())
}

#[tokio::test]
async fn test_tar_import_export() -> Result<()> {
    let fixture = Fixture::new()?;
    let srcdir = &fixture.path.join("src");
    std::fs::create_dir(srcdir)?;
    let src_tar = tokio::fs::File::open(&generate_test_tarball(srcdir)?).await?;

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

fn skopeo_inspect(imgref: &str) -> Result<String> {
    let out = Command::new("skopeo")
        .args(&["inspect", imgref])
        .stdout(std::process::Stdio::piped())
        .output()?;
    Ok(String::from_utf8(out.stdout)?)
}

#[tokio::test]
async fn test_container_import_export() -> Result<()> {
    let cancellable = gio::NONE_CANCELLABLE;
    let fixture = Fixture::new()?;
    let srcdir = &fixture.path.join("src");
    std::fs::create_dir(srcdir)?;
    let srcrepopath = &generate_test_repo(srcdir)?;
    let srcrepo = &ostree::Repo::new_for_path(srcrepopath);
    srcrepo.open(cancellable)?;
    let testrev = srcrepo
        .resolve_rev(TESTREF, false)
        .context("Failed to resolve ref")?
        .unwrap();

    let srcoci_path = &srcdir.join("oci");
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
    let digest = ostree_ext::container::export(srcrepo, TESTREF, &config, &srcoci_imgref)
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
    let r = ostree_ext::container::import(&fixture.destrepo, &srcoci_unknownremote, None)
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
        p = srcdir.as_str()
    )?;

    // No remote matching
    let srcoci_verified = OstreeImageReference {
        sigverify: SignatureSource::OstreeRemote("myremote".to_string()),
        imgref: srcoci_imgref.clone(),
    };
    let import = ostree_ext::container::import(&fixture.destrepo, &srcoci_verified, None)
        .await
        .context("importing")?;
    assert_eq!(import.ostree_commit, testrev.as_str());

    // Test without signature verification
    // Create a new repo
    let fixture = Fixture::new()?;
    let import = ostree_ext::container::import(&fixture.destrepo, &srcoci_unverified, None)
        .await
        .context("importing")?;
    assert_eq!(import.ostree_commit, testrev.as_str());

    Ok(())
}

/// We should currently reject an image with multiple layers.
#[tokio::test]
async fn test_container_import_derive() -> Result<()> {
    let fixture = Fixture::new()?;
    let exampleos_path = &fixture.path.join("exampleos.ociarchive");
    std::fs::write(exampleos_path, EXAMPLEOS_DERIVED_OCI)?;
    let exampleos_ref = OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: ImageReference {
            transport: Transport::OciArchive,
            name: exampleos_path.to_string(),
        },
    };
    let r = ostree_ext::container::import(&fixture.destrepo, &exampleos_ref, None).await;
    assert_err_contains(r, "Expected 1 layer, found 2");
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
