use crate::ipc::ClientToDaemonConnection;
use anyhow::Result;
use structopt::StructOpt;

/// `bootupctl` sub-commands.
#[derive(Debug, StructOpt)]
#[structopt(name = "bootupctl", about = "Bootupd client application")]
pub enum CtlCommand {
    #[structopt(name = "status", about = "Show components status")]
    Status(StatusOpts),
    #[structopt(name = "update", about = "Update all components")]
    Update,
    #[structopt(name = "validate", about = "Validate system state")]
    Validate,
}

#[derive(Debug, StructOpt)]
pub struct StatusOpts {
    // Output JSON
    #[structopt(long)]
    json: bool,
}

impl CtlCommand {
    /// Run CLI application.
    pub fn run(self) -> Result<()> {
        match self {
            CtlCommand::Status(opts) => Self::run_status(opts),
            CtlCommand::Update => Self::run_update(),
            CtlCommand::Validate => Self::run_validate(),
        }
    }

    /// Runner for `status` verb.
    fn run_status(opts: StatusOpts) -> Result<()> {
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

    /// Runner for `validate` verb.
    fn run_validate() -> Result<()> {
        let mut client = ClientToDaemonConnection::new();
        client.connect()?;
        crate::client_run_validate(&mut client)?;
        client.shutdown()?;
        Ok(())
    }
}
