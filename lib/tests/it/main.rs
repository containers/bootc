use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use indoc::indoc;
use ostree_ext::container::{Config, ImageReference, Transport};
use ostree_ext::gio;
use sh_inline::bash;
use std::{io::Write, process::Command};

const EXAMPLEOS_V0: &[u8] = include_bytes!("fixtures/exampleos.tar.zst");
const EXAMPLEOS_V1: &[u8] = include_bytes!("fixtures/exampleos-v1.tar.zst");
const TESTREF: &str = "exampleos/x86_64/stable";
const EXAMPLEOS_CONTENT_CHECKSUM: &str =
    "0ef7461f9db15e1d8bd8921abf20694225fbaa4462cadf7deed8ea0e43162120";

#[context("Generating test repo")]
fn generate_test_repo(dir: &Utf8Path) -> Result<Utf8PathBuf> {
    let src_tarpath = &dir.join("exampleos.tar.zst");
    std::fs::write(src_tarpath, EXAMPLEOS_V0)?;

    bash!(
        indoc! {"
        cd {dir}
        ostree --repo=repo init --mode=archive
        ostree --repo=repo commit -b {testref} --bootable --add-metadata-string=version=42.0 --add-detached-metadata-string=my-detached-key=my-detached-value --tree=tar=exampleos.tar.zst
        ostree --repo=repo show {testref}
    "},
        testref = TESTREF,
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

#[tokio::test]
async fn test_tar_import_export() -> Result<()> {
    let cancellable = gio::NONE_CANCELLABLE;

    let tempdir = tempfile::tempdir_in("/var/tmp")?;
    let path = Utf8Path::from_path(tempdir.path()).unwrap();
    let srcdir = &path.join("src");
    std::fs::create_dir(srcdir)?;
    let src_tar = tokio::fs::File::open(&generate_test_tarball(srcdir)?).await?;
    let destdir = &path.join("dest");
    std::fs::create_dir(destdir)?;
    let destrepodir = &destdir.join("repo");
    let destrepo = ostree::Repo::new_for_path(destrepodir);
    destrepo.create(ostree::RepoMode::BareUser, cancellable)?;

    let imported_commit: String = ostree_ext::tar::import_tar(&destrepo, src_tar).await?;
    let (commitdata, _) = destrepo.load_commit(&imported_commit)?;
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
        destrepodir = destrepodir.as_str(),
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

    let tempdir = tempfile::tempdir_in("/var/tmp")?;
    let path = Utf8Path::from_path(tempdir.path()).unwrap();
    let srcdir = &path.join("src");
    std::fs::create_dir(srcdir)?;
    let destdir = &path.join("dest");
    std::fs::create_dir(destdir)?;
    let srcrepopath = &generate_test_repo(srcdir)?;
    let srcrepo = &ostree::Repo::new_for_path(srcrepopath);
    srcrepo.open(cancellable)?;
    let testrev = srcrepo
        .resolve_rev(TESTREF, false)
        .context("Failed to resolve ref")?
        .unwrap();
    let destrepo = &ostree::Repo::new_for_path(destdir);
    destrepo.create(ostree::RepoMode::BareUser, cancellable)?;

    let srcoci_path = &srcdir.join("oci");
    let srcoci = ImageReference {
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
    let pushed = ostree_ext::container::export(srcrepo, TESTREF, &config, &srcoci)
        .await
        .context("exporting")?;
    assert!(srcoci_path.exists());
    let digest = pushed.name.rsplitn(2, '@').next().unwrap();

    let inspect = skopeo_inspect(&srcoci.to_string())?;
    assert!(inspect.contains(r#""version": "42.0""#));
    assert!(inspect.contains(r#""foo": "bar""#));
    assert!(inspect.contains(r#""test": "value""#));

    let inspect = ostree_ext::container::fetch_manifest_info(&srcoci).await?;
    assert_eq!(inspect.manifest_digest, digest);

    let import = ostree_ext::container::import(destrepo, &srcoci, None)
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
