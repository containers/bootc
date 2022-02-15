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
use std::io::Write;
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
    pub(crate) srcrepo: ostree::Repo,
    pub(crate) destrepo: ostree::Repo,

    pub(crate) format_version: u32,
}

impl Fixture {
    #[context("Initializing fixture")]
    pub(crate) fn new_base() -> Result<Self> {
        // Basic setup, allocate a tempdir
        let tempdir = tempfile::tempdir_in("/var/tmp")?;
        let dir = Arc::new(cap_std::fs::Dir::open_ambient_dir(
            tempdir.path(),
            cap_std::ambient_authority(),
        )?);
        let path: &Utf8Path = tempdir.path().try_into().unwrap();
        let path = path.to_path_buf();

        // Create the src/ directory
        dir.create_dir("src")?;
        let srcdir_dfd = &dir.open_dir("src")?;

        // Initialize the src/gpghome/ directory
        let gpgtarname = "gpghome.tgz";
        srcdir_dfd.write(gpgtarname, OSTREE_GPG_HOME)?;
        let gpgtar = srcdir_dfd.open(gpgtarname)?;
        srcdir_dfd.remove_file(gpgtarname)?;
        srcdir_dfd.create_dir("gpghome")?;
        let gpghome = srcdir_dfd.open_dir("gpghome")?;
        let st = std::process::Command::new("tar")
            .cwd_dir_owned(gpghome)
            .stdin(Stdio::from(gpgtar))
            .args(&["-azxf", "-"])
            .status()?;
        assert!(st.success());

        let srcrepo =
            ostree::Repo::create_at_dir(srcdir_dfd, "repo", ostree::RepoMode::Archive, None)
                .context("Creating src/ repo")?;

        dir.create_dir("dest")?;
        let destrepo =
            ostree::Repo::create_at_dir(&dir, "dest/repo", ostree::RepoMode::BareUser, None)?;
        Ok(Self {
            _tempdir: tempdir,
            dir,
            path,
            srcrepo,
            destrepo,
            format_version: 0,
        })
    }

    pub(crate) fn new() -> Result<Self> {
        let r = Self::new_base()?;
        let tarname = "exampleos.tar.zst";
        r.dir.write(tarname, EXAMPLEOS_V0)?;
        bash_in!(
            r.dir,
            indoc! {"
            ostree --repo=src/repo commit -b ${testref} --bootable --no-bindings --add-metadata=ostree.container-cmd='[\"/usr/bin/bash\"]' --add-metadata-string=version=42.0 --add-metadata-string=buildsys.checksum=41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3 --gpg-homedir=src/gpghome --gpg-sign=${keyid} \
              --add-detached-metadata-string=my-detached-key=my-detached-value --tree=tar=exampleos.tar.zst >/dev/null
            ostree --repo=src/repo show ${testref} >/dev/null
        "},
            testref = r.testref(),
            keyid = TEST_GPG_KEYID_1
        ).context("Writing commit")?;
        r.dir.remove_file(tarname)?;
        Ok(r)
    }

    pub(crate) fn testref(&self) -> &'static str {
        TESTREF
    }

    #[context("Updating test repo")]
    pub(crate) fn update(&mut self) -> Result<()> {
        let tmptarpath = "src/repo/tmp/exampleos-v1.tar.zst";
        self.dir.write(tmptarpath, EXAMPLEOS_V1)?;
        let testref = TESTREF;
        bash_in!(
            &self.dir,
            "ostree --repo=src/repo commit -b ${testref} --no-bindings --tree=tar=${tmptarpath}",
            testref,
            tmptarpath
        )?;
        self.dir.remove_file(tmptarpath)?;
        Ok(())
    }

    #[context("Exporting tar")]
    pub(crate) fn export_tar(&self) -> Result<&'static Utf8Path> {
        let cancellable = gio::NONE_CANCELLABLE;
        let (_, rev) = self.srcrepo.read_commit(self.testref(), cancellable)?;
        let path = "exampleos-export.tar";
        let mut outf = std::io::BufWriter::new(self.dir.create(path)?);
        let options = ostree_ext::tar::ExportOptions {
            format_version: self.format_version,
            ..Default::default()
        };
        ostree_ext::tar::export_commit(&self.srcrepo, rev.as_str(), &mut outf, Some(options))?;
        outf.flush()?;
        Ok(path.into())
    }
}
