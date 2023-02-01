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

impl Bios {
    // Run grub2-install
    fn run_grub_install(&self, dest_root: &str, device: &str) -> Result<()> {
        let grub_install = Path::new("/").join(GRUB_BIN);
        if !grub_install.exists() {
            bail!("Failed to find {:?}", grub_install);
        }

        let mut cmd = Command::new(grub_install);
        let boot_dir = Path::new(dest_root).join("boot");
        #[cfg(target_arch = "x86_64")]
        cmd.args(&["--target", "i386-pc"])
            .args(&["--boot-directory", boot_dir.to_str().unwrap()])
            .args(&["--modules", "mdraid1x"])
            .arg(device);

        #[cfg(target_arch = "powerpc64")]
        cmd.args(&["--target", "powerpc-ieee1275"])
            .args(&["--boot-directory", boot_dir.to_str().unwrap()])
            .arg("--no-nvram")
            .arg(device);

        let cmdout = cmd.output()?;
        if !cmdout.status.success() {
            std::io::stderr().write_all(&cmdout.stderr)?;
            bail!("Failed to run {:?}", cmd);
        }
        Ok(())
    }
}

impl Component for Bios {
    fn name(&self) -> &'static str {
        "BIOS"
    }

    fn install(
        &self,
        src_root: &openat::Dir,
        dest_root: &str,
        device: &str,
    ) -> Result<InstalledContent> {
        let meta = if let Some(meta) = get_component_update(src_root, self)? {
            meta
        } else {
            anyhow::bail!("No update metadata for component {} found", self.name());
        };

        self.run_grub_install(dest_root, device)?;
        Ok(InstalledContent {
            meta,
            filetree: None,
            adopted_from: None,
        })
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