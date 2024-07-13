use anyhow::Result;
use camino::Utf8Path;
use fn_error_context::context;

use crate::blockdev::Device;
use crate::task::Task;

/// The name of the mountpoint for efi (as a subdirectory of /boot, or at the toplevel)
pub(crate) const EFI_DIR: &str = "efi";

#[context("Installing bootloader")]
pub(crate) fn install_via_bootupd(
    device: &Device,
    rootfs: &Utf8Path,
    configopts: &crate::install::InstallConfigOpts,
) -> Result<()> {
    let verbose = std::env::var_os("BOOTC_BOOTLOADER_DEBUG").map(|_| "-vvvv");
    // bootc defaults to only targeting the platform boot method.
    let bootupd_opts = (!configopts.generic_image).then_some(["--update-firmware", "--auto"]);
    let devpath = device.path();
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
