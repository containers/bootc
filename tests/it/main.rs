use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use indoc::indoc;
use sh_inline::bash;
use std::io::Write;

const EXAMPLEOS_TAR: &[u8] = include_bytes!("fixtures/exampleos.tar.zst");
const TESTREF: &str = "exampleos/x86_64/stable";
const CONTENT_CHECKSUM: &str = "0ef7461f9db15e1d8bd8921abf20694225fbaa4462cadf7deed8ea0e43162120";

#[context("Generating test OCI")]
fn generate_test_tarball(dir: &Utf8Path) -> Result<Utf8PathBuf> {
    let cancellable = gio::NONE_CANCELLABLE;
    let path = Utf8Path::new(dir);
    let src_tarpath = &path.join("exampleos.tar.zst");
    std::fs::write(src_tarpath, EXAMPLEOS_TAR)?;
    bash!(
        indoc! {"
        cd {path}
        ostree --repo=repo-archive init --mode=archive
        ostree --repo=repo-archive commit -b {testref} --tree=tar=exampleos.tar.zst
        ostree --repo=repo-archive show {testref}
    "},
        testref = TESTREF,
        path = path.as_str()
    )?;
    std::fs::remove_file(src_tarpath)?;
    let repopath = &path.join("repo-archive");
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, repopath.as_str(), cancellable)?;
    let (_, rev) = repo.read_commit(TESTREF, cancellable)?;
    let (commitv, _) = repo.load_commit(rev.as_str())?;
    assert_eq!(
        ostree::commit_get_content_checksum(&commitv)
            .unwrap()
            .as_str(),
        CONTENT_CHECKSUM
    );
    let destpath = path.join("exampleos-export.tar");
    let mut outf = std::io::BufWriter::new(std::fs::File::create(&destpath)?);
    ostree_ext::tar::export_commit(repo, rev.as_str(), &mut outf)?;
    outf.flush()?;
    Ok(destpath)
}

#[test]
fn test_e2e() -> Result<()> {
    let cancellable = gio::NONE_CANCELLABLE;

    let tempdir = tempfile::tempdir()?;
    let path = Utf8Path::from_path(tempdir.path()).unwrap();
    let srcdir = &path.join("src");
    std::fs::create_dir(srcdir)?;
    let src_tar =
        &mut std::io::BufReader::new(std::fs::File::open(&generate_test_tarball(srcdir)?)?);
    let destdir = &path.join("dest");
    std::fs::create_dir(destdir)?;
    let destrepodir = &destdir.join("repo");
    let destrepo = ostree::Repo::new_for_path(destrepodir);
    destrepo.create(ostree::RepoMode::Archive, cancellable)?;

    let imported_commit: String = ostree_ext::tar::import_tar(&destrepo, src_tar)?;
    let (commitdata, _) = destrepo.load_commit(&imported_commit)?;
    assert_eq!(
        CONTENT_CHECKSUM,
        ostree::commit_get_content_checksum(&commitdata)
            .unwrap()
            .as_str()
    );
    bash!(
        "ostree --repo={destrepodir} ls -R {imported_commit}",
        destrepodir = destrepodir.as_str(),
        imported_commit = imported_commit.as_str()
    )?;
    Ok(())
}
