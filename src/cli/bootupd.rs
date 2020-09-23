use anyhow::{Context, Result};
use structopt::StructOpt;

/// `bootupd` sub-commands.
#[derive(Debug, StructOpt)]
#[structopt(name = "bootupd", about = "Bootupd backend commands")]
pub enum DCommand {
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
        match self {
            DCommand::Daemon => crate::daemon(),
            DCommand::Install(opts) => Self::run_install(opts),
            DCommand::GenerateUpdateMetadata(opts) => Self::run_generate_meta(opts),
        }
    }

    /// Runner for `generate-install-metadata` verb.
    fn run_generate_meta(opts: GenerateOpts) -> Result<()> {
        crate::generate_update_metadata(&opts.sysroot).context("generating metadata failed")?;
        Ok(())
    }

    /// Runner for `install` verb.
    fn run_install(opts: InstallOpts) -> Result<()> {
        crate::install(&opts.src_root, &opts.dest_root).context("boot data installation failed")?;
        Ok(())
    }
}
