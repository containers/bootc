//! Command-line interface (CLI) logic.

use anyhow::Result;
use clap::Parser;
use log::LevelFilter;
mod bootupctl;
mod bootupd;

/// Top-level multicall CLI.
#[derive(Debug, Parser)]
pub enum MultiCall {
    Ctl(bootupctl::CtlCommand),
    D(bootupd::DCommand),
}

impl MultiCall {
    pub fn from_args(args: Vec<String>) -> Self {
        use std::os::unix::ffi::OsStrExt;

        // This is a multicall binary, dispatched based on the introspected
        // filename found in argv[0].
        let exe_name = {
            let arg0 = args.get(0).cloned().unwrap_or_default();
            let exe_path = std::path::PathBuf::from(arg0);
            exe_path.file_name().unwrap_or_default().to_os_string()
        };
        #[allow(clippy::wildcard_in_or_patterns)]
        match exe_name.as_bytes() {
            b"bootupctl" => MultiCall::Ctl(bootupctl::CtlCommand::parse_from(args)),
            b"bootupd" | _ => MultiCall::D(bootupd::DCommand::parse_from(args)),
        }
    }

    pub fn run(self) -> Result<()> {
        match self {
            MultiCall::Ctl(ctl_cmd) => ctl_cmd.run(),
            MultiCall::D(d_cmd) => d_cmd.run(),
        }
    }

    /// Return the log-level set via command-line flags.
    pub fn loglevel(&self) -> LevelFilter {
        match self {
            MultiCall::Ctl(cmd) => cmd.loglevel(),
            MultiCall::D(cmd) => cmd.loglevel(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_apps() {
        use clap::CommandFactory;
        bootupctl::CtlCommand::command().debug_assert();
        bootupd::DCommand::command().debug_assert();
    }

    #[test]
    fn test_multicall_dispatch() {
        {
            let d_argv = vec![
                "/usr/bin/bootupd".to_string(),
                "generate-update-metadata".to_string(),
            ];
            let cli = MultiCall::from_args(d_argv);
            match cli {
                MultiCall::Ctl(cmd) => panic!("{:?}", cmd),
                MultiCall::D(_) => {}
            };
        }
        {
            let ctl_argv = vec!["/usr/bin/bootupctl".to_string(), "validate".to_string()];
            let cli = MultiCall::from_args(ctl_argv);
            match cli {
                MultiCall::Ctl(_) => {}
                MultiCall::D(cmd) => panic!("{:?}", cmd),
            };
        }
        {
            let ctl_argv = vec!["/bin-mount/bootupctl".to_string(), "validate".to_string()];
            let cli = MultiCall::from_args(ctl_argv);
            match cli {
                MultiCall::Ctl(_) => {}
                MultiCall::D(cmd) => panic!("{:?}", cmd),
            };
        }
    }

    #[test]
    fn test_verbosity() {
        let default = MultiCall::from_args(vec![
            "bootupd".to_string(),
            "generate-update-metadata".to_string(),
        ]);
        assert_eq!(default.loglevel(), LevelFilter::Warn);

        let info = MultiCall::from_args(vec![
            "bootupd".to_string(),
            "generate-update-metadata".to_string(),
            "-v".to_string(),
        ]);
        assert_eq!(info.loglevel(), LevelFilter::Info);
    }
}
