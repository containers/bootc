use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::fs::Dir;
use cap_std_ext::prelude::CapStdExtCommandExt;
use fn_error_context::context;
use indoc::indoc;
use ostree::cap_std;
use ostree_ext::gio;
use sh_inline::bash_in;
use std::convert::TryInto;
use std::process::Stdio;
use std::sync::Arc;

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
    pub(crate) dir: Arc<Dir>,
    pub(crate) path: Utf8PathBuf,
    pub(crate) srcdir: Utf8PathBuf,
    pub(crate) srcrepo: ostree::Repo,
    pub(crate) destrepo: ostree::Repo,
    pub(crate) destrepo_path: Utf8PathBuf,

    pub(crate) format_version: u32,
}

impl Fixture {
    pub(crate) fn new() -> Result<Self> {
        let tempdir = tempfile::tempdir_in("/var/tmp")?;
        let dir = Arc::new(cap_std::fs::Dir::open_ambient_dir(
            tempdir.path(),
            cap_std::ambient_authority(),
        )?);
        let path: &Utf8Path = tempdir.path().try_into().unwrap();
        let path = path.to_path_buf();

        let srcdir = path.join("src");
        std::fs::create_dir(&srcdir)?;
        let srcdir_dfd = &dir.open_dir("src")?;
        generate_test_repo(srcdir_dfd, TESTREF)?;
        let srcrepo = ostree::Repo::open_at_dir(srcdir_dfd, "repo")?;

        let destdir = &path.join("dest");
        std::fs::create_dir(destdir)?;
        let destrepo_path = destdir.join("repo");
        let destrepo = ostree::Repo::new_for_path(&destrepo_path);
        destrepo.create(ostree::RepoMode::BareUser, gio::NONE_CANCELLABLE)?;
        Ok(Self {
            _tempdir: tempdir,
            dir,
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

    #[context("Updating test repo")]
    pub(crate) fn update(&mut self) -> Result<()> {
        let repopath = &self.srcdir.join("repo");
        let repotmp = &repopath.join("tmp");
        let srcpath = &repotmp.join("exampleos-v1.tar.zst");
        std::fs::write(srcpath, EXAMPLEOS_V1)?;
        let srcpath = srcpath.as_str();
        let testref = TESTREF;
        bash_in!(
            self.dir.open_dir("src")?,
            "ostree --repo=repo commit -b ${testref} --no-bindings --tree=tar=${srcpath}",
            testref,
            srcpath
        )?;
        std::fs::remove_file(srcpath)?;
        Ok(())
    }
}

#[context("Generating test repo")]
pub(crate) fn generate_test_repo(dir: &Dir, testref: &str) -> Result<()> {
    let gpgtarname = "gpghome.tgz";
    dir.write(gpgtarname, OSTREE_GPG_HOME)?;
    let gpgtar = dir.open(gpgtarname)?;
    dir.remove_file(gpgtarname)?;

    dir.create_dir("gpghome")?;
    let gpghome = dir.open_dir("gpghome")?;
    let st = std::process::Command::new("tar")
        .cwd_dir_owned(gpghome)
        .stdin(Stdio::from(gpgtar))
        .args(&["-azxf", "-"])
        .status()?;
    assert!(st.success());
    let tarname = "exampleos.tar.zst";
    dir.write(tarname, EXAMPLEOS_V0)?;
    bash_in!(
        dir,
        indoc! {"
        ostree --repo=repo init --mode=archive
        ostree --repo=repo commit -b ${testref} --bootable --no-bindings --add-metadata=ostree.container-cmd='[\"/usr/bin/bash\"]' --add-metadata-string=version=42.0 --add-metadata-string=buildsys.checksum=41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3 --gpg-homedir=gpghome --gpg-sign=${keyid} \
          --add-detached-metadata-string=my-detached-key=my-detached-value --tree=tar=exampleos.tar.zst >/dev/null
        ostree --repo=repo show ${testref} >/dev/null
    "},
        testref = testref,
        keyid = TEST_GPG_KEYID_1
    ).context("Writing commit")?;
    dir.remove_file(tarname)?;
    Ok(())
}
