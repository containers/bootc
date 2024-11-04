use std::io::prelude::*;
use std::path::Path;
use std::process::Command;

use crate::component::*;
use crate::model::*;
use crate::packagesystem;
use anyhow::{bail, Result};

use crate::util;
use serde::{Deserialize, Serialize};

// grub2-install file path
pub(crate) const GRUB_BIN: &str = "usr/sbin/grub2-install";

#[derive(Serialize, Deserialize, Debug)]
struct BlockDevice {
    path: String,
    pttype: Option<String>,
    parttypename: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Devices {
    blockdevices: Vec<BlockDevice>,
}

#[derive(Default)]
pub(crate) struct Bios {}

impl Bios {
    // get target device for running update
    fn get_device(&self) -> Result<String> {
        let mut cmd: Command;
        #[cfg(target_arch = "x86_64")]
        {
            // find /boot partition
            cmd = Command::new("findmnt");
            cmd.arg("--noheadings")
                .arg("--output")
                .arg("SOURCE")
                .arg("/boot");
            let partition = util::cmd_output(&mut cmd)?;

            // lsblk to find parent device
            cmd = Command::new("lsblk");
            cmd.arg("--paths")
                .arg("--noheadings")
                .arg("--output")
                .arg("PKNAME")
                .arg(partition.trim());
        }

        #[cfg(target_arch = "powerpc64")]
        {
            // get PowerPC-PReP-boot partition
            cmd = Command::new("realpath");
            cmd.arg("/dev/disk/by-partlabel/PowerPC-PReP-boot");
        }

        let device = util::cmd_output(&mut cmd)?;
        Ok(device)
    }

    // Run grub2-install
    fn run_grub_install(&self, dest_root: &str, device: &str) -> Result<()> {
        let grub_install = Path::new("/").join(GRUB_BIN);
        if !grub_install.exists() {
            bail!("Failed to find {:?}", grub_install);
        }

        let mut cmd = Command::new(grub_install);
        let boot_dir = Path::new(dest_root).join("boot");
        // We forcibly inject mdraid1x because it's needed by CoreOS's default of "install raw disk image"
        // We also add part_gpt because in some cases probing of the partition map can fail such
        // as in a container, but we always use GPT.
        #[cfg(target_arch = "x86_64")]
        cmd.args(["--target", "i386-pc"])
            .args(["--boot-directory", boot_dir.to_str().unwrap()])
            .args(["--modules", "mdraid1x part_gpt"])
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

    // check bios_boot partition on gpt type disk
    fn get_bios_boot_partition(&self) -> Result<Option<String>> {
        let target = self.get_device()?;
        // lsblk to list children with bios_boot
        let output = Command::new("lsblk")
            .args([
                "--json",
                "--output",
                "PATH,PTTYPE,PARTTYPENAME",
                target.trim(),
            ])
            .output()?;
        if !output.status.success() {
            std::io::stderr().write_all(&output.stderr)?;
            bail!("Failed to run lsblk");
        }

        let output = String::from_utf8(output.stdout)?;
        // Parse the JSON string into the `Devices` struct
        let Ok(devices) = serde_json::from_str::<Devices>(&output) else {
            bail!("Could not deserialize JSON output from lsblk");
        };

        // Find the device with the parttypename "BIOS boot"
        for device in devices.blockdevices {
            if let Some(parttypename) = &device.parttypename {
                if parttypename == "BIOS boot" && device.pttype.as_deref() == Some("gpt") {
                    return Ok(Some(device.path));
                }
            }
        }
        Ok(None)
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
        _update_firmware: bool,
    ) -> Result<InstalledContent> {
        let Some(meta) = get_component_update(src_root, self)? else {
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
        let meta = packagesystem::query_files(sysroot_path, [&grub_install])?;
        write_update_metadata(sysroot_path, self, &meta)?;
        Ok(meta)
    }

    fn query_adopt(&self) -> Result<Option<Adoptable>> {
        #[cfg(target_arch = "x86_64")]
        if crate::efi::is_efi_booted()? && self.get_bios_boot_partition()?.is_none() {
            log::debug!("Skip BIOS adopt");
            return Ok(None);
        }
        crate::component::query_adopt_state()
    }

    fn adopt_update(&self, _: &openat::Dir, update: &ContentMetadata) -> Result<InstalledContent> {
        let Some(meta) = self.query_adopt()? else {
            anyhow::bail!("Failed to find adoptable system")
        };

        let device = self.get_device()?;
        let device = device.trim();
        self.run_grub_install("/", device)?;
        Ok(InstalledContent {
            meta: update.clone(),
            filetree: None,
            adopted_from: Some(meta.version),
        })
    }

    fn query_update(&self, sysroot: &openat::Dir) -> Result<Option<ContentMetadata>> {
        get_component_update(sysroot, self)
    }

    fn run_update(&self, sysroot: &openat::Dir, _: &InstalledContent) -> Result<InstalledContent> {
        let updatemeta = self.query_update(sysroot)?.expect("update available");
        let device = self.get_device()?;
        let device = device.trim();
        self.run_grub_install("/", device)?;

        let adopted_from = None;
        Ok(InstalledContent {
            meta: updatemeta,
            filetree: None,
            adopted_from,
        })
    }

    fn validate(&self, _: &InstalledContent) -> Result<ValidationResult> {
        Ok(ValidationResult::Skip)
    }

    fn get_efi_vendor(&self, _: &openat::Dir) -> Result<Option<String>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_lsblk_output() {
        let data = include_str!("../tests/fixtures/example-lsblk-output.json");
        let devices: Devices = serde_json::from_str(&data).expect("JSON was not well-formatted");
        assert_eq!(devices.blockdevices.len(), 7);
        assert_eq!(devices.blockdevices[0].path, "/dev/sr0");
        assert!(devices.blockdevices[0].pttype.is_none());
        assert!(devices.blockdevices[0].parttypename.is_none());
    }
}
