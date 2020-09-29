//! Bootupd command-line application.

use structopt::clap::crate_name;

/// Binary entrypoint, for both daemon and client logic.
fn main() {
    let exit_code = run_cli();
    std::process::exit(exit_code);
}

/// CLI logic.
fn run_cli() -> i32 {
    // Parse command-line options.
    let args: Vec<_> = std::env::args().collect();
    let cli_opts = bootupd::MultiCall::from_args(args);

    // Setup logging.
    env_logger::Builder::from_default_env()
        .format_timestamp(None)
        .format_module_path(false)
        .filter(Some(crate_name!()), cli_opts.loglevel())
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
