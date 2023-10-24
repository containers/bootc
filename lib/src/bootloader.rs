use std::os::unix::prelude::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std::fs::Dir;
use cap_std::fs::Permissions;
use cap_std_ext::cap_std;
use cap_std_ext::prelude::*;
use fn_error_context::context;

use crate::task::Task;

const GRUB_BOOT_UUID_FILE: &str = "bootuuid.cfg";
/// The name of the mountpoint for efi (as a subdirectory of /boot, or at the toplevel)
pub(crate) const EFI_DIR: &str = "efi";

/// Return `true` if the system is booted via EFI
pub(crate) fn is_efi_booted() -> Result<bool> {
    if !super::install::ARCH_USES_EFI {
        return Ok(false);
    }
    Path::new("/sys/firmware/efi")
        .try_exists()
        .map_err(Into::into)
}

#[context("Installing bootloader")]
pub(crate) fn install_via_bootupd(
    device: &Utf8Path,
    rootfs: &Utf8Path,
    boot_uuid: &str,
    is_alongside: bool,
) -> Result<()> {
    let verbose = std::env::var_os("BOOTC_BOOTLOADER_DEBUG").map(|_| "-vvvv");
    // If we're doing an alongside install, only match the boot method because Anaconda defaults
    // to only doing that.  This is only on x86_64 because that's the only arch that has multiple
    // components right now.
    // TODO: Add --component=auto which moves this logic into bootupd
    let component_args = if cfg!(target_arch = "x86_64") && is_alongside {
        assert!(super::install::ARCH_USES_EFI);
        let install_efi = is_efi_booted()?;
        let component_arg = if install_efi {
            "--component=EFI"
        } else {
            "--component=BIOS"
        };
        Some(component_arg)
    } else {
        None
    };
    let args = ["backend", "install", "--with-static-configs"]
        .into_iter()
        .chain(verbose)
        .chain(component_args)
        .chain([
            "--src-root",
            "/",
            "--device",
            device.as_str(),
            rootfs.as_str(),
        ]);
    Task::new_and_run("Running bootupctl to install bootloader", "bootupctl", args)?;

    let grub2_uuid_contents = format!("set BOOT_UUID=\"{boot_uuid}\"\n");

    let bootfs = &rootfs.join("boot");
    let bootfs =
        Dir::open_ambient_dir(bootfs, cap_std::ambient_authority()).context("Opening boot")?;
    let grub2 = bootfs.open_dir("grub2").context("Opening boot/grub2")?;

    grub2
        .atomic_write_with_perms(
            GRUB_BOOT_UUID_FILE,
            grub2_uuid_contents,
            Permissions::from_mode(0o644),
        )
        .with_context(|| format!("Writing {GRUB_BOOT_UUID_FILE}"))?;

    Ok(())
}
