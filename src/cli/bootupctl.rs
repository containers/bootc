use crate::ipc::ClientToDaemonConnection;
use anyhow::Result;
use structopt::clap::AppSettings;
use structopt::StructOpt;

/// `bootupctl` sub-commands.
#[derive(Debug, StructOpt)]
#[structopt(name = "bootupctl", about = "Bootupd client application")]
pub enum CtlCommand {
    // FIXME(lucab): drop this after refreshing
    // https://github.com/coreos/fedora-coreos-config/pull/595
    #[structopt(name = "backend", setting = AppSettings::Hidden)]
    Backend(CtlBackend),
    #[structopt(name = "status", about = "Show components status")]
    Status(StatusOpts),
    #[structopt(name = "update", about = "Update all components")]
    Update,
    #[structopt(name = "validate", about = "Validate system state")]
    Validate,
}

#[derive(Debug, StructOpt)]
pub enum CtlBackend {
    #[structopt(name = "generate-update-metadata", setting = AppSettings::Hidden)]
    Generate(super::bootupd::GenerateOpts),
    #[structopt(name = "install", setting = AppSettings::Hidden)]
    Install(super::bootupd::InstallOpts),
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
            CtlCommand::Backend(CtlBackend::Generate(opts)) => {
                super::bootupd::DCommand::run_generate_meta(opts)
            }
            CtlCommand::Backend(CtlBackend::Install(opts)) => {
                super::bootupd::DCommand::run_install(opts)
            }
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
