/*!
**Boot**loader **upd**ater.

This is an early prototype hidden/not-yet-standardized mechanism
which just updates EFI for now (x86_64/aarch64 only).

But in the future will hopefully gain some independence from
ostree and also support e.g. updating the MBR etc.

Refs:
 * <https://github.com/coreos/fedora-coreos-tracker/issues/510>
!*/

#![deny(unused_must_use)]
// The style lints are more annoying than useful
#![allow(clippy::style)]

mod backend;
#[cfg(any(target_arch = "x86_64", target_arch = "powerpc64"))]
mod bios;
mod bootupd;
mod cli;
mod component;
mod coreos;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod efi;
mod failpoints;
mod filesystem;
mod filetree;
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "powerpc64"
))]
mod grubconfigs;
mod model;
mod model_legacy;
mod ostreeutil;
mod packagesystem;
mod sha512string;
mod util;

pub const BACKEND_NAME: &str = "bootupd";
pub const CLIENT_NAME: &str = "bootupctl";

pub fn run<T>(args: impl IntoIterator<Item = T>) -> anyhow::Result<()>
where
    T: Into<std::ffi::OsString> + Clone,
{
    let _scenario = fail::FailScenario::setup();
    let cli_opts = cli::MultiCall::from_args(args);

    // Dispatch CLI subcommand.
    cli_opts.run()
}
