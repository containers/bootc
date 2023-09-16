use std::os::unix::prelude::PermissionsExt;

use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std::fs::Dir;
use cap_std::fs::Permissions;
use cap_std_ext::cap_std;
use cap_std_ext::cap_std::fs::DirBuilder;
use cap_std_ext::prelude::*;
use fn_error_context::context;

use crate::task::Task;

/// This variable is referenced by our GRUB fragment
pub(crate) const IGNITION_VARIABLE: &str = "$ignition_firstboot";
const GRUB_BOOT_UUID_FILE: &str = "bootuuid.cfg";
const STATIC_GRUB_CFG: &str = include_str!("grub.cfg");
const STATIC_GRUB_CFG_EFI: &str = include_str!("grub-efi.cfg");
/// The name of the mountpoint for efi (as a subdirectory of /boot, or at the toplevel)
pub(crate) const EFI_DIR: &str = "efi";

fn install_grub2_efi(efidir: &Dir, uuid: &str) -> Result<()> {
    let mut vendordir = None;
    let efidir = efidir.open_dir("EFI").context("Opening EFI/")?;
    for child in efidir.entries()? {
        let child = child?;
        let name = child.file_name();
        let name = if let Some(name) = name.to_str() {
            name
        } else {
            continue;
        };
        if name == "BOOT" {
            continue;
        }
        if !child.file_type()?.is_dir() {
            continue;
        }
        vendordir = Some(child.open_dir()?);
        break;
    }
    let vendordir = vendordir.ok_or_else(|| anyhow::anyhow!("Failed to find EFI vendor dir"))?;
    vendordir
        .atomic_write("grub.cfg", STATIC_GRUB_CFG_EFI)
        .context("Writing static EFI grub.cfg")?;
    vendordir
        .atomic_write(GRUB_BOOT_UUID_FILE, uuid)
        .with_context(|| format!("Writing {GRUB_BOOT_UUID_FILE}"))?;

    Ok(())
}

#[context("Installing bootloader")]
pub(crate) fn install_via_bootupd(
    device: &Utf8Path,
    rootfs: &Utf8Path,
    boot_uuid: &str,
) -> Result<()> {
    Task::new_and_run(
        "Running bootupctl to install bootloader",
        "bootupctl",
        [
            "backend",
            "install",
            "--src-root",
            "/",
            "--device",
            device.as_str(),
            rootfs.as_str(),
        ],
    )?;

    let grub2_uuid_contents = format!("set BOOT_UUID=\"{boot_uuid}\"\n");

    let bootfs = &rootfs.join("boot");
    let bootfs = Dir::open_ambient_dir(bootfs, cap_std::ambient_authority())?;

    if super::install::ARCH_USES_EFI {
        let efidir = bootfs.open_dir("efi")?;
        install_grub2_efi(&efidir, &grub2_uuid_contents)?;
    }

    bootfs.ensure_dir_with("grub2", &DirBuilder::new())?;
    let grub2 = bootfs.open_dir("grub2")?;

    // Mode 0700 to support passwords etc.
    grub2.set_permissions(".", Permissions::from_mode(0o700))?;
    grub2
        .atomic_write_with_perms(
            "grub.cfg",
            STATIC_GRUB_CFG,
            cap_std::fs::Permissions::from_mode(0o600),
        )
        .context("Writing grub.cfg")?;

    grub2
        .atomic_write_with_perms(
            GRUB_BOOT_UUID_FILE,
            grub2_uuid_contents,
            Permissions::from_mode(0o644),
        )
        .with_context(|| format!("Writing {GRUB_BOOT_UUID_FILE}"))?;

    Ok(())
}
