use crate::bootupd;
use anyhow::{Context, Result};
use clap::Parser;
use log::LevelFilter;

/// `bootupd` sub-commands.
#[derive(Debug, Parser)]
#[clap(name = "bootupd", about = "Bootupd backend commands", version)]
pub struct DCommand {
    /// Verbosity level (higher is more verbose).
    #[clap(short = 'v', action = clap::ArgAction::Count, global = true)]
    verbosity: u8,

    /// CLI sub-command.
    #[clap(subcommand)]
    pub cmd: DVerb,
}

impl DCommand {
    /// Return the log-level set via command-line flags.
    pub(crate) fn loglevel(&self) -> LevelFilter {
        match self.verbosity {
            0 => LevelFilter::Warn,
            1 => LevelFilter::Info,
            2 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        }
    }
}

/// CLI sub-commands.
#[derive(Debug, Parser)]
pub enum DVerb {
    #[clap(name = "daemon", about = "Run service logic")]
    Daemon,
    #[clap(name = "generate-update-metadata", about = "Generate metadata")]
    GenerateUpdateMetadata(GenerateOpts),
    #[clap(name = "install", about = "Install components")]
    Install(InstallOpts),
}

#[derive(Debug, Parser)]
pub struct InstallOpts {
    /// Source root
    #[clap(long, value_parser, default_value_t = String::from("/"))]
    src_root: String,
    /// Target root
    #[clap(value_parser)]
    dest_root: String,

    /// Target device, used by bios bootloader installation
    #[clap(long)]
    device: Option<String>,

    /// Enable installation of the built-in static config files
    #[clap(long)]
    with_static_configs: bool,

    #[clap(long = "component")]
    /// Only install these components
    components: Option<Vec<String>>,
}

#[derive(Debug, Parser)]
pub struct GenerateOpts {
    /// Physical root mountpoint
    #[clap(value_parser)]
    sysroot: Option<String>,
}

impl DCommand {
    /// Run CLI application.
    pub fn run(self) -> Result<()> {
        match self.cmd {
            DVerb::Daemon => crate::daemon::run(),
            DVerb::Install(opts) => Self::run_install(opts),
            DVerb::GenerateUpdateMetadata(opts) => Self::run_generate_meta(opts),
        }
    }

    /// Runner for `generate-install-metadata` verb.
    pub(crate) fn run_generate_meta(opts: GenerateOpts) -> Result<()> {
        let sysroot = opts.sysroot.as_deref().unwrap_or("/");
        if sysroot != "/" {
            anyhow::bail!("Using a non-default sysroot is not supported: {}", sysroot);
        }
        bootupd::generate_update_metadata(sysroot).context("generating metadata failed")?;
        Ok(())
    }

    /// Runner for `install` verb.
    pub(crate) fn run_install(opts: InstallOpts) -> Result<()> {
        bootupd::install(
            &opts.src_root,
            &opts.dest_root,
            opts.device.as_deref(),
            opts.with_static_configs,
            opts.components.as_deref(),
        )
        .context("boot data installation failed")?;
        Ok(())
    }
}
