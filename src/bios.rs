use std::io::prelude::*;
use std::path::Path;

use crate::component::*;
use crate::model::*;
use anyhow::{bail, Result};

use crate::util;

// grub2-install file path
pub(crate) const GRUB_BIN: &str = "usr/sbin/grub2-install";

#[derive(Default)]
pub(crate) struct Bios {}

impl Component for Bios {
    fn name(&self) -> &'static str {
        "BIOS"
    }

    fn install(&self, src_root: &openat::Dir, dest_root: &str) -> Result<InstalledContent> {
        todo!();
    }

    fn generate_update_metadata(&self, sysroot_path: &str) -> Result<ContentMetadata> {
        let grub_install = Path::new(sysroot_path).join(GRUB_BIN);
        if !grub_install.exists() {
            bail!("Failed to find {:?}", grub_install);
        }

        // Query the rpm database and list the package and build times for /usr/sbin/grub2-install
        let mut rpmout = util::rpm_query(sysroot_path, &grub_install)?;
        let rpmout = rpmout.output()?;
        if !rpmout.status.success() {
            std::io::stderr().write_all(&rpmout.stderr)?;
            bail!("Failed to invoke rpm -qf");
        }

        let meta = util::parse_rpm_metadata(rpmout.stdout)?;
        write_update_metadata(sysroot_path, self, &meta)?;
        Ok(meta)
    }

    fn query_adopt(&self) -> Result<Option<Adoptable>> {
        todo!();
    }

    fn adopt_update(
        &self,
        sysroot: &openat::Dir,
        update: &ContentMetadata,
    ) -> Result<InstalledContent> {
        todo!();
    }

    fn query_update(&self, sysroot: &openat::Dir) -> Result<Option<ContentMetadata>> {
        todo!();
    }

    fn run_update(
        &self,
        sysroot: &openat::Dir,
        current: &InstalledContent,
    ) -> Result<InstalledContent> {
        todo!();
    }

    fn validate(&self, current: &InstalledContent) -> Result<ValidationResult> {
        todo!();
    }
}