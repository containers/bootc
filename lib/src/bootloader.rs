use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;

use crate::blockdev::PartitionTable;
use crate::task::Task;

/// The name of the mountpoint for efi (as a subdirectory of /boot, or at the toplevel)
pub(crate) const EFI_DIR: &str = "efi";
pub(crate) const PREPBOOT_GUID: &str = "9E1A2D38-C612-4316-AA26-8B49521E5A8B";
pub(crate) const PREPBOOT_LABEL: &str = "PowerPC-PReP-boot";

/// Find the device to pass to bootupd. Only on powerpc64 right now
/// we explicitly find one with a specific label.
///
/// This should get fixed once we execute on https://github.com/coreos/bootupd/issues/432
fn get_bootupd_device(device: &PartitionTable) -> Result<Utf8PathBuf> {
    #[cfg(target_arch = "powerpc64")]
    {
        return device
            .partitions
            .iter()
            .find(|p| p.parttype.as_str() == PREPBOOT_GUID)
            .ok_or_else(|| {
                anyhow::anyhow!("Failed to find PReP partition with GUID {PREPBOOT_GUID}")
            })
            .map(|dev| dev.node.as_str().into());
    }
    #[cfg(not(target_arch = "powerpc64"))]
    return Ok(device.path().into());
}

#[context("Installing bootloader")]
pub(crate) fn install_via_bootupd(
    device: &PartitionTable,
    rootfs: &Utf8Path,
    configopts: &crate::install::InstallConfigOpts,
) -> Result<()> {
    let verbose = std::env::var_os("BOOTC_BOOTLOADER_DEBUG").map(|_| "-vvvv");
    // bootc defaults to only targeting the platform boot method.
    let bootupd_opts = (!configopts.generic_image).then_some(["--update-firmware", "--auto"]);

    let devpath = get_bootupd_device(device)?;
    let args = ["backend", "install", "--write-uuid"]
        .into_iter()
        .chain(verbose)
        .chain(bootupd_opts.iter().copied().flatten())
        .chain(["--device", devpath.as_str(), rootfs.as_str()]);
    Task::new("Running bootupctl to install bootloader", "bootupctl")
        .args(args)
        .verbose()
        .run()
}
