use anyhow::{Context, Result};
use log::LevelFilter;
use structopt::StructOpt;

/// `bootupd` sub-commands.
#[derive(Debug, StructOpt)]
#[structopt(name = "bootupd", about = "Bootupd backend commands")]
pub struct DCommand {
    /// Verbosity level (higher is more verbose).
    #[structopt(short = "v", parse(from_occurrences), global = true)]
    verbosity: u8,

    /// CLI sub-command.
    #[structopt(subcommand)]
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
#[derive(Debug, StructOpt)]
pub enum DVerb {
    #[structopt(name = "daemon", about = "Run service logic")]
    Daemon,
    #[structopt(name = "generate-update-metadata", about = "Generate metadata")]
    GenerateUpdateMetadata(GenerateOpts),
    #[structopt(name = "install", about = "Install components")]
    Install(InstallOpts),
}

#[derive(Debug, StructOpt)]
pub struct InstallOpts {
    /// Source root
    #[structopt(long, default_value = "/")]
    src_root: String,
    /// Target root
    dest_root: String,
}

#[derive(Debug, StructOpt)]
pub struct GenerateOpts {
    /// Physical root mountpoint
    sysroot: String,
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
        crate::generate_update_metadata(&opts.sysroot).context("generating metadata failed")?;
        Ok(())
    }

    /// Runner for `install` verb.
    pub(crate) fn run_install(opts: InstallOpts) -> Result<()> {
        crate::install(&opts.src_root, &opts.dest_root).context("boot data installation failed")?;
        Ok(())
    }
}
