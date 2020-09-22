//! Bootupd command-line application.

use log::LevelFilter;
use structopt::clap::crate_name;
use structopt::StructOpt;

/// Binary entrypoint, for both daemon and client logic.
fn main() {
    let exit_code = run_cli();
    std::process::exit(exit_code);
}

/// CLI logic.
fn run_cli() -> i32 {
    // Parse command-line options.
    let cli_opts = bootupd::CliOptions::from_args();

    // Setup logging.
    env_logger::Builder::from_default_env()
        .format_timestamp(None)
        .format_module_path(false)
        .filter(Some(crate_name!()), LevelFilter::Warn)
        .init();

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
