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

use clap::crate_name;

/// Binary entrypoint, for both daemon and client logic.
fn main() {
    let _scenario = fail::FailScenario::setup();
    let exit_code = run_cli();
    std::process::exit(exit_code);
}

/// CLI logic.
fn run_cli() -> i32 {
    // Parse command-line options.
    let args: Vec<_> = std::env::args().collect();
    let cli_opts = cli::MultiCall::from_args(args);

    // Setup logging.
    env_logger::Builder::from_default_env()
        .format_timestamp(None)
        .format_module_path(false)
        .filter(Some(crate_name!()), cli_opts.loglevel())
        .init();

    log::trace!("executing cli");

    // Dispatch CLI subcommand.
    match cli_opts.run() {
        Ok(_) => libc::EXIT_SUCCESS,
        Err(e) => {
            // Use the alternative formatter to get everything on a single line... it reads better.
            eprintln!("error: {:#}", e);
            libc::EXIT_FAILURE
        }
    }
}
