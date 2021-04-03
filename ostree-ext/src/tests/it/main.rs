use std::{fs::File, io::BufReader};

use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use indoc::indoc;
use sh_inline::bash;

use ostree_container::oci as myoci;

const EXAMPLEOS_TAR: &[u8] = include_bytes!("fixtures/exampleos.tar.zst");
const TESTREF: &str = "exampleos/x86_64/stable";
const CONTENT_CHECKSUM: &str = "0ef7461f9db15e1d8bd8921abf20694225fbaa4462cadf7deed8ea0e43162120";

#[context("Generating test OCI")]
fn generate_test_oci(dir: &Utf8Path) -> Result<Utf8PathBuf> {
    let cancellable = gio::NONE_CANCELLABLE;
    let path = Utf8Path::new(dir);
    let tarpath = &path.join("exampleos.tar.zst");
    std::fs::write(tarpath, EXAMPLEOS_TAR)?;
    bash!(
        indoc! {"
        cd {path}
        ostree --repo=repo-archive init --mode=archive
        ostree --repo=repo-archive commit -b {testref} --tree=tar=exampleos.tar.zst
        ostree --repo=repo-archive show {testref}
        ostree --repo=repo-archive ls -R -X -C {testref}
    "},
        testref = TESTREF,
        path = path.as_str()
    )?;
    std::fs::remove_file(tarpath)?;
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
    let ocipath = path.join("exampleos-oci");
    let ocitarget = ostree_container::buildoci::Target::OciDir(ocipath.as_ref());
    ostree_container::buildoci::build(repo, TESTREF, ocitarget)?;
    bash!(r"skopeo inspect oci:{ocipath}", ocipath = ocipath.as_str())?;
    Ok(ocipath)
}

fn read_blob(ocidir: &Utf8Path, digest: &str) -> Result<BufReader<File>> {
    let digest = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("Unknown algorithim in digest {}", digest))?;
    let f = File::open(ocidir.join("blobs/sha256").join(digest))
        .with_context(|| format!("Opening blob {}", digest))?;
    Ok(std::io::BufReader::new(f))
}

#[context("Parsing OCI")]
fn find_layer_in_oci(ocidir: &Utf8Path) -> Result<BufReader<File>> {
    let f = std::io::BufReader::new(
        File::open(ocidir.join("index.json")).context("Opening index.json")?,
    );
    let index: myoci::Index = serde_json::from_reader(f)?;
    let manifest = index
        .manifests
        .get(0)
        .ok_or_else(|| anyhow!("Missing manifest in index.json"))?;
    let f = read_blob(ocidir, &manifest.digest)?;
    let manifest: myoci::Manifest = serde_json::from_reader(f)?;
    let layer = manifest
        .layers
        .iter()
        .find(|layer| {
            matches!(
                layer.media_type.as_str(),
                myoci::DOCKER_TYPE_LAYER | oci_distribution::manifest::IMAGE_LAYER_GZIP_MEDIA_TYPE
            )
        })
        .ok_or_else(|| anyhow!("Failed to find rootfs layer"))?;
    Ok(read_blob(ocidir, &layer.digest)?)
}

#[test]
fn test_e2e() -> Result<()> {
    let cancellable = gio::NONE_CANCELLABLE;

    let tempdir = tempfile::tempdir()?;
    let path = Utf8Path::from_path(tempdir.path()).unwrap();
    let srcdir = &path.join("src");
    std::fs::create_dir(srcdir)?;
    let ocidir = &generate_test_oci(srcdir)?;
    let destdir = &path.join("dest");
    std::fs::create_dir(destdir)?;
    let destrepodir = &destdir.join("repo");
    let destrepo = ostree::Repo::new_for_path(destrepodir);
    destrepo.create(ostree::RepoMode::Archive, cancellable)?;

    let tarf = find_layer_in_oci(ocidir)?;
    let imported_commit = ostree_container::client::import_tarball(&destrepo, tarf)?;
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
