use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use indoc::indoc;
use ostree_ext::gio;
use sh_inline::bash;
use std::convert::TryInto;

const OSTREE_GPG_HOME: &[u8] = include_bytes!("fixtures/ostree-gpg-test-home.tar.gz");
const TEST_GPG_KEYID_1: &str = "7FCA23D8472CDAFA";
#[allow(dead_code)]
const TEST_GPG_KEYFPR_1: &str = "5E65DE75AB1C501862D476347FCA23D8472CDAFA";
pub(crate) const EXAMPLEOS_V0: &[u8] = include_bytes!("fixtures/exampleos.tar.zst");
pub(crate) const EXAMPLEOS_V1: &[u8] = include_bytes!("fixtures/exampleos-v1.tar.zst");
const TESTREF: &str = "exampleos/x86_64/stable";

pub(crate) struct Fixture {
    // Just holds a reference
    _tempdir: tempfile::TempDir,
    pub(crate) path: Utf8PathBuf,
    pub(crate) srcdir: Utf8PathBuf,
    pub(crate) srcrepo: ostree::Repo,
    pub(crate) destrepo: ostree::Repo,
    pub(crate) destrepo_path: Utf8PathBuf,

    pub(crate) format_version: u32,
}

impl Fixture {
    pub(crate) fn new() -> Result<Self> {
        let _tempdir = tempfile::tempdir_in("/var/tmp")?;
        let path: &Utf8Path = _tempdir.path().try_into().unwrap();
        let path = path.to_path_buf();

        let srcdir = path.join("src");
        std::fs::create_dir(&srcdir)?;
        let srcrepo_path = generate_test_repo(&srcdir, TESTREF)?;
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

    pub(crate) fn testref(&self) -> &'static str {
        TESTREF
    }

    pub(crate) fn update(&mut self) -> Result<()> {
        let repopath = &self.srcdir.join("repo");
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
}

#[context("Generating test repo")]
pub(crate) fn generate_test_repo(dir: &Utf8Path, testref: &str) -> Result<Utf8PathBuf> {
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
        ostree --repo=repo commit -b {testref} --bootable --no-bindings --add-metadata=ostree.container-cmd='[\"/usr/bin/bash\"]' --add-metadata-string=version=42.0 --add-metadata-string=buildsys.checksum=41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3 --gpg-homedir={gpghome} --gpg-sign={keyid} \
          --add-detached-metadata-string=my-detached-key=my-detached-value --tree=tar=exampleos.tar.zst >/dev/null
        ostree --repo=repo show {testref} >/dev/null
    "},
        testref = testref,
        gpghome = gpghome.as_str(),
        keyid = TEST_GPG_KEYID_1,
        dir = dir.as_str()
    )?;
    std::fs::remove_file(src_tarpath)?;
    Ok(dir.join("repo"))
}
