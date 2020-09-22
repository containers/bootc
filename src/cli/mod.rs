//! Command-line interface (CLI) logic.

use crate::ipc::ClientToDaemonConnection;
use anyhow::{Context, Result};
use structopt::StructOpt;

/// Top-level CLI options.
#[derive(Debug, StructOpt)]
pub struct CliOptions {
    /// CLI sub-commands.
    #[structopt(subcommand)]
    pub(crate) cmd: CliCommand,
}

impl CliOptions {
    /// Run CLI application.
    pub fn run(self) -> Result<()> {
        match self.cmd {
            CliCommand::Backend(opts) => run_backend(opts),
            CliCommand::Daemon => crate::daemon(),
            CliCommand::Status(opts) => run_status(opts),
            CliCommand::Update => run_update(),
        }
    }
}

/// CLI sub-commands.
#[derive(Debug, StructOpt)]
pub(crate) enum CliCommand {
    #[structopt(name = "backend")]
    Backend(crate::BackendOpt),
    #[structopt(name = "daemon")]
    Daemon,
    #[structopt(name = "status")]
    Status(crate::StatusOptions),
    #[structopt(name = "update")]
    Update,
}

/// Runner for `backend` verb.
fn run_backend(opts: crate::BackendOpt) -> Result<()> {
    use crate::BackendOpt;

    match opts {
        BackendOpt::Install {
            src_root,
            dest_root,
        } => crate::install(&src_root, &dest_root).context("boot data installation failed")?,
        BackendOpt::GenerateUpdateMetadata { sysroot } => {
            crate::generate_update_metadata(&sysroot).context("generating metadata failed")?
        }
    };

    Ok(())
}

/// Runner for `status` verb.
fn run_status(opts: crate::StatusOptions) -> Result<()> {
    let mut client = ClientToDaemonConnection::new();
    client.connect()?;

    let r: crate::Status = client.send(&crate::ClientRequest::Status)?;
    if opts.json {
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        serde_json::to_writer_pretty(&mut stdout, &r)?;
    } else {
        crate::print_status(&r);
    }

    client.shutdown()?;

    Ok(())
}

/// Runner for `update` verb.
fn run_update() -> Result<()> {
    let mut client = ClientToDaemonConnection::new();
    client.connect()?;

    crate::client_run_update(&mut client)?;

    client.shutdown()?;
    Ok(())
}
