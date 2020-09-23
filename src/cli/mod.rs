//! Command-line interface (CLI) logic.

use anyhow::Result;
use structopt::StructOpt;

mod bootupctl;
mod bootupd;

/// Top-level multicall CLI.
#[derive(Debug, StructOpt)]
pub enum MultiCall {
    Ctl(bootupctl::CtlCommand),
    D(bootupd::DCommand),
}

impl MultiCall {
    pub fn from_args() -> Self {
        use std::os::unix::ffi::OsStrExt;

        // This is a multicall binary, dispatched based on the introspected
        // filename found in argv[0].
        let exe_name = {
            let arg0 = std::env::args().nth(0).unwrap_or_default();
            let exe_path = std::path::PathBuf::from(arg0);
            exe_path.file_name().unwrap_or_default().to_os_string()
        };
        match exe_name.as_bytes() {
            b"bootupctl" => MultiCall::Ctl(bootupctl::CtlCommand::from_args()),
            b"bootupd" | _ => MultiCall::D(bootupd::DCommand::from_args()),
        }
    }

    pub fn run(self) -> Result<()> {
        match self {
            MultiCall::Ctl(ctl_cmd) => ctl_cmd.run(),
            MultiCall::D(d_cmd) => d_cmd.run(),
        }
    }
}
