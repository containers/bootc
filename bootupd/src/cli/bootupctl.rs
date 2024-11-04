use crate::bootupd;
use anyhow::Result;
use clap::Parser;
use log::LevelFilter;

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

static SYSTEMD_ARGS_BOOTUPD: &[&str] = &[
    "--unit",
    "bootupd",
    "--property",
    "PrivateNetwork=yes",
    "--property",
    "ProtectHome=yes",
    "--property",
    "MountFlags=slave",
    "--pipe",
];

/// `bootupctl` sub-commands.
#[derive(Debug, Parser)]
#[clap(name = "bootupctl", about = "Bootupd client application", version)]
pub struct CtlCommand {
    /// Verbosity level (higher is more verbose).
    #[clap(short = 'v', action = clap::ArgAction::Count, global = true)]
    verbosity: u8,

    /// CLI sub-command.
    #[clap(subcommand)]
    pub cmd: CtlVerb,
}

impl CtlCommand {
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
pub enum CtlVerb {
    // FIXME(lucab): drop this after refreshing
    // https://github.com/coreos/fedora-coreos-config/pull/595
    #[clap(name = "backend", hide = true, subcommand)]
    Backend(CtlBackend),
    #[clap(name = "status", about = "Show components status")]
    Status(StatusOpts),
    #[clap(name = "update", about = "Update all components")]
    Update,
    #[clap(name = "adopt-and-update", about = "Update all adoptable components")]
    AdoptAndUpdate,
    #[clap(name = "validate", about = "Validate system state")]
    Validate,
}

#[derive(Debug, Parser)]
pub enum CtlBackend {
    #[clap(name = "generate-update-metadata", hide = true)]
    Generate(super::bootupd::GenerateOpts),
    #[clap(name = "install", hide = true)]
    Install(super::bootupd::InstallOpts),
}

#[derive(Debug, Parser)]
pub struct StatusOpts {
    /// If there are updates available, output `Updates available: ` to standard output;
    /// otherwise output nothing.  Avoid parsing this, just check whether or not
    /// the output is empty.
    #[clap(long, action)]
    print_if_available: bool,

    /// Output JSON
    #[clap(long, action)]
    json: bool,
}

impl CtlCommand {
    /// Run CLI application.
    pub fn run(self) -> Result<()> {
        match self.cmd {
            CtlVerb::Status(opts) => Self::run_status(opts),
            CtlVerb::Update => Self::run_update(),
            CtlVerb::AdoptAndUpdate => Self::run_adopt_and_update(),
            CtlVerb::Validate => Self::run_validate(),
            CtlVerb::Backend(CtlBackend::Generate(opts)) => {
                super::bootupd::DCommand::run_generate_meta(opts)
            }
            CtlVerb::Backend(CtlBackend::Install(opts)) => {
                super::bootupd::DCommand::run_install(opts)
            }
        }
    }

    /// Runner for `status` verb.
    fn run_status(opts: StatusOpts) -> Result<()> {
        ensure_running_in_systemd()?;
        let r = bootupd::status()?;
        if opts.json {
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            serde_json::to_writer_pretty(&mut stdout, &r)?;
        } else if opts.print_if_available {
            bootupd::print_status_avail(&r)?;
        } else {
            bootupd::print_status(&r)?;
        }

        Ok(())
    }

    /// Runner for `update` verb.
    fn run_update() -> Result<()> {
        ensure_running_in_systemd()?;
        bootupd::client_run_update()
    }

    /// Runner for `update` verb.
    fn run_adopt_and_update() -> Result<()> {
        ensure_running_in_systemd()?;
        bootupd::client_run_adopt_and_update()
    }

    /// Runner for `validate` verb.
    fn run_validate() -> Result<()> {
        ensure_running_in_systemd()?;
        bootupd::client_run_validate()
    }
}

/// Checks if the current process is (apparently at least)
/// running under systemd.
fn running_in_systemd() -> bool {
    std::env::var_os("INVOCATION_ID").is_some()
}

/// Require root permission
fn require_root_permission() -> Result<()> {
    if !rustix::process::getuid().is_root() {
        anyhow::bail!("This command requires root privileges")
    }
    Ok(())
}

/// Detect if we're running in systemd; if we're not, we re-exec ourselves via
/// systemd-run. Then we can just directly run code in what is now the daemon.
fn ensure_running_in_systemd() -> Result<()> {
    require_root_permission()?;
    let running_in_systemd = running_in_systemd();
    if !running_in_systemd {
        // Clear any failure status that may have happened previously
        let _r = Command::new("systemctl")
            .arg("reset-failed")
            .arg("bootupd.service")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?
            .wait()?;
        let r = Command::new("systemd-run")
            .args(SYSTEMD_ARGS_BOOTUPD)
            .args(std::env::args())
            .exec();
        // If we got here, it's always an error
        return Err(r.into());
    }
    Ok(())
}
