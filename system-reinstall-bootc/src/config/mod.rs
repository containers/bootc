use anyhow::{ensure, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};

mod cli;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReinstallConfig {
    /// The bootc image to install on the system.
    pub(crate) bootc_image: String,

    /// The raw CLI arguments that were used to invoke the program. None if the config was loaded
    /// from a file.
    #[serde(skip_deserializing)]
    cli_flags: Option<Vec<String>>,
}

impl ReinstallConfig {
    pub fn parse_from_cli(cli: cli::Cli) -> Self {
        Self {
            bootc_image: cli.bootc_image,
            cli_flags: Some(std::env::args().collect::<Vec<String>>()),
        }
    }

    pub fn load() -> Result<Self> {
        Ok(match std::env::var("BOOTC_REINSTALL_CONFIG") {
            Ok(config_path) => {
                ensure_no_cli_args()?;

                serde_yaml::from_slice(
                    &std::fs::read(&config_path)
                        .context("reading BOOTC_REINSTALL_CONFIG file {config_path}")?,
                )
                .context("parsing BOOTC_REINSTALL_CONFIG file {config_path}")?
            }
            Err(_) => ReinstallConfig::parse_from_cli(cli::Cli::parse()),
        })
    }
}

fn ensure_no_cli_args() -> Result<()> {
    let num_args = std::env::args().len();

    ensure!(
        num_args == 1,
        "BOOTC_REINSTALL_CONFIG is set, but there are {num_args} CLI arguments. BOOTC_REINSTALL_CONFIG is meant to be used with no arguments."
    );

    Ok(())
}
