/*!
**Boot**loader **upd**ater.

This is an early prototype hidden/not-yet-standardized mechanism
which just updates EFI for now (x86_64/aarch64 only).

But in the future will hopefully gain some independence from
ostree and also support e.g. updating the MBR etc.

Refs:
 * <https://github.com/coreos/fedora-coreos-tracker/issues/510>
!*/

#![deny(unused_must_use)]

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use nix::sys::socket as nixsocket;
use openat_ext::OpenatDirExt;
use serde::{Deserialize, Serialize};
use std::io::prelude::*;
use std::path::Path;
use structopt::StructOpt;

// #[cfg(any(target_arch = "x86_64"))]
// mod bios;
mod cli;
pub use cli::CliOptions;
mod component;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod efi;
mod filetree;
mod ipc;
use component::*;
mod model;
use model::*;
mod ostreeutil;
mod sha512string;
mod util;

/// Stored in /boot to describe our state; think of it like
/// a tiny rpm/dpkg database.  It's stored in /boot
pub(crate) const STATEFILE_DIR: &str = "boot";
pub(crate) const STATEFILE_NAME: &str = "bootupd-state.json";
pub(crate) const WRITE_LOCK_PATH: &str = "run/bootupd-lock";

#[derive(Debug, Serialize, Deserialize, StructOpt)]
#[structopt(rename_all = "kebab-case")]
struct StatusOptions {
    // Output JSON
    #[structopt(long)]
    json: bool,
}

// Options exposed by `bootupctl backend`
#[derive(Debug, Serialize, Deserialize, StructOpt)]
#[structopt(name = "boot-update")]
#[structopt(rename_all = "kebab-case")]
enum BackendOpt {
    /// Install data from available components into a disk image
    Install {
        /// Source root
        #[structopt(long, default_value = "/")]
        src_root: String,
        /// Target root
        dest_root: String,
    },
    /// Install data from available components into a filesystem tree
    GenerateUpdateMetadata {
        /// Physical root mountpoint
        sysroot: String,
    },
}

// "end user" options, i.e. what people should run on client systems
#[derive(Debug, Serialize, Deserialize, StructOpt)]
#[structopt(name = "boot-update")]
#[structopt(rename_all = "kebab-case")]
enum Opt {
    /// Update all components
    Update,
    /// Print the current state
    Status(StatusOptions),
}

/// A message sent from client to server
#[derive(Debug, Serialize, Deserialize)]
enum ClientRequest {
    /// Update a component
    Update { component: String },
    /// Print the current state
    Status,
}

pub(crate) fn install(source_root: &str, dest_root: &str) -> Result<()> {
    let statepath = Path::new(dest_root)
        .join(STATEFILE_DIR)
        .join(STATEFILE_NAME);
    if statepath.exists() {
        bail!("{:?} already exists, cannot re-install", statepath);
    }

    let components = get_components();
    if components.is_empty() {
        println!("No components available for this platform.");
        return Ok(());
    }
    let mut state = SavedState {
        installed: Default::default(),
        pending: Default::default(),
    };
    for component in components {
        let meta = component.install(source_root, dest_root)?;
        state.installed.insert(component.name().into(), meta);
    }

    let sysroot = openat::Dir::open(dest_root)?;
    update_state(&sysroot, &state)?;

    Ok(())
}

pub(crate) fn get_components() -> Vec<Box<dyn Component>> {
    let mut components: Vec<Box<dyn Component>> = Vec::new();

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    components.push(Box::new(efi::EFI::new()));

    // #[cfg(target_arch = "x86_64")]
    // components.push(Box::new(bios::BIOS::new()));

    components
}

pub(crate) fn generate_update_metadata(sysroot_path: &str) -> Result<()> {
    for component in get_components() {
        let v = component.generate_update_metadata(sysroot_path)?;
        println!(
            "Generated update layout for {}: {}",
            component.name(),
            format_version(&v)
        );
    }

    Ok(())
}

/// Hold a lock on the system root; while ordinarily we run
/// as a systemd unit which implicitly ensures a "singleton"
/// instance this is a double check.
fn acquire_write_lock<P: AsRef<Path>>(sysroot: P) -> Result<std::fs::File> {
    let sysroot = sysroot.as_ref();
    let lockf = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(sysroot.join(WRITE_LOCK_PATH))?;
    lockf.lock_exclusive()?;
    Ok(lockf)
}

/// Return value from daemon → client for component update
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
enum ComponentUpdateResult {
    AtLatestVersion,
    Updated {
        previous: ContentMetadata,
        interrupted: Option<ContentMetadata>,
        new: ContentMetadata,
    },
}

/// daemon implementation of component update
fn update(name: &str) -> Result<ComponentUpdateResult> {
    let sysroot = openat::Dir::open("/")?;
    let _lock = acquire_write_lock("/")?;
    let mut state = get_saved_state("/")?.unwrap_or_else(|| SavedState {
        ..Default::default()
    });
    let component = component::new_from_name(name)?;
    let inst = if let Some(inst) = state.installed.get(name) {
        inst.clone()
    } else {
        anyhow::bail!("Component {} is not installed", name);
    };
    let update = component.query_update()?;
    let update = match update.as_ref() {
        Some(p) if !p.compare(&inst.meta) => p,
        _ => return Ok(ComponentUpdateResult::AtLatestVersion),
    };
    let mut pending_container = state.pending.take().unwrap_or_default();
    let interrupted = pending_container.get(component.name()).cloned();

    pending_container.insert(component.name().into(), update.clone());
    update_state(&sysroot, &state)?;
    let newinst = component
        .run_update(&inst)
        .with_context(|| format!("Failed to update {}", component.name()))?;
    state.installed.insert(component.name().into(), newinst);
    pending_container.remove(component.name());
    update_state(&sysroot, &state)?;
    Ok(ComponentUpdateResult::Updated {
        previous: inst.meta,
        interrupted,
        new: update.clone(),
    })
}

/// Atomically replace the on-disk state with a new version
fn update_state(sysroot_dir: &openat::Dir, state: &SavedState) -> Result<()> {
    let subdir = sysroot_dir.sub_dir(STATEFILE_DIR)?;
    let f = {
        let f = subdir.new_unnamed_file(0o644)?;
        let mut buff = std::io::BufWriter::new(f);
        serde_json::to_writer(&mut buff, state)?;
        buff.flush()?;
        buff.into_inner()?
    };
    let dest_tmp_name = {
        // expect OK because we just created the filename above from a constant
        let mut buf = std::ffi::OsString::from(STATEFILE_NAME);
        buf.push(".tmp");
        buf
    };
    let dest_tmp_name = Path::new(&dest_tmp_name);
    if subdir.exists(dest_tmp_name)? {
        subdir.remove_file(dest_tmp_name)?;
    }
    subdir.link_file_at(&f, dest_tmp_name)?;
    f.sync_all()?;
    subdir.local_rename(dest_tmp_name, STATEFILE_NAME)?;
    Ok(())
}

/// Load the JSON file containing on-disk state
fn get_saved_state(sysroot_path: &str) -> Result<Option<SavedState>> {
    let sysroot_dir = openat::Dir::open(sysroot_path)
        .with_context(|| format!("opening sysroot {}", sysroot_path))?;

    let statefile_path = Path::new(STATEFILE_DIR).join(STATEFILE_NAME);
    let saved_state = if let Some(statusf) = sysroot_dir.open_file_optional(&statefile_path)? {
        let bufr = std::io::BufReader::new(statusf);
        let saved_state: SavedState = serde_json::from_reader(bufr)?;
        Some(saved_state)
    } else {
        None
    };
    Ok(saved_state)
}

/// Print a version if available, or fall back to timestamp
fn format_version(meta: &ContentMetadata) -> String {
    if let Some(version) = meta.version.as_ref() {
        version.into()
    } else {
        meta.timestamp.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    }
}

fn status() -> Result<Status> {
    let mut ret: Status = Default::default();
    let state = if let Some(state) = get_saved_state("/")? {
        state
    } else {
        return Ok(ret);
    };
    for (name, ic) in state.installed.iter() {
        let component = component::new_from_name(&name)?;
        let component = component.as_ref();
        let interrupted = state
            .pending
            .as_ref()
            .map(|p| p.get(name.as_str()))
            .flatten();
        let update = component.query_update()?;
        let updatable = match update.as_ref() {
            Some(p) if !p.compare(&ic.meta) => true,
            _ => false,
        };
        ret.components.insert(
            name.to_string(),
            ComponentStatus {
                installed: ic.meta.clone(),
                interrupted: interrupted.cloned(),
                update,
                updatable,
            },
        );
    }
    Ok(ret)
}

fn daemon_process_one(client: &mut ipc::AuthenticatedClient) -> Result<()> {
    let mut buf = [0u8; ipc::MSGSIZE];
    loop {
        let n = nixsocket::recv(client.fd, &mut buf, nixsocket::MsgFlags::MSG_CMSG_CLOEXEC)?;
        let buf = &buf[0..n];
        if buf.is_empty() {
            println!("Client disconnected");
            break;
        }

        let msg = bincode::deserialize(&buf)?;
        let r = match msg {
            ClientRequest::Update { component } => {
                println!("Processing update");
                bincode::serialize(&match update(component.as_str()) {
                    Ok(v) => ipc::DaemonToClientReply::Success::<ComponentUpdateResult>(v),
                    Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
                })?
            }
            ClientRequest::Status => {
                println!("Processing status");
                bincode::serialize(&match status() {
                    Ok(v) => ipc::DaemonToClientReply::Success::<Status>(v),
                    Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
                })?
            }
        };
        let written = nixsocket::send(client.fd, &r, nixsocket::MsgFlags::MSG_CMSG_CLOEXEC)?;
        if written != r.len() {
            bail!("Wrote {} bytes to client, expected {}", written, r.len());
        }
    }
    Ok(())
}

fn daemon() -> Result<()> {
    use libsystemd::daemon::{self, NotifyState};
    use std::os::unix::io::IntoRawFd;
    if !daemon::booted() {
        bail!("Not running systemd")
    }
    let mut fds = libsystemd::activation::receive_descriptors(true)
        .map_err(|e| anyhow::anyhow!("Failed to receieve systemd descriptors: {}", e))?;
    let srvsock_fd = if let Some(fd) = fds.pop() {
        fd
    } else {
        bail!("No fd passed from systemd");
    };
    let srvsock_fd = srvsock_fd.into_raw_fd();
    let sent = daemon::notify(true, &[NotifyState::Ready]).expect("notify failed");
    if !sent {
        bail!("Failed to notify systemd");
    }
    loop {
        let client = ipc::UnauthenticatedClient::new(nixsocket::accept4(
            srvsock_fd,
            nixsocket::SockFlag::SOCK_CLOEXEC,
        )?);
        let mut client = client.authenticate()?;
        daemon_process_one(&mut client)?;
    }
}

fn print_status(status: &Status) {
    for (name, component) in status.components.iter() {
        println!("Component {}", name);
        println!("  Installed: {}", format_version(&component.installed));

        if let Some(i) = component.interrupted.as_ref() {
            println!(
                "  WARNING: Previous update to {} was interrupted",
                format_version(i)
            );
        }
        if component.updatable {
            let update = component.update.as_ref().expect("update");
            println!("  Update: Available: {}", format_version(&update));
        } else if component.update.is_some() {
            println!("  Update: At latest version");
        } else {
            println!("  Update: No update found");
        }
    }
}

/// Checks that the user has provided an environment variable to signal
/// acceptance of our alpha state - use this when performing write operations.
fn validate_preview_env() -> Result<()> {
    let v = "BOOTUPD_ACCEPT_PREVIEW";
    if std::env::var_os(v).is_none() {
        Err(anyhow::anyhow!(
            "bootupd is currently alpha; set {}=1 in environment to continue",
            v
        ))
    } else {
        Ok(())
    }
}

fn client_run_update(c: &mut ipc::ClientToDaemonConnection) -> Result<()> {
    validate_preview_env()?;
    let status: Status = c.send(&ClientRequest::Status)?;
    if status.components.is_empty() {
        println!("No components installed.");
        return Ok(());
    }
    let mut updated = false;
    for (name, cstatus) in status.components.iter() {
        if !cstatus.updatable {
            continue;
        }
        match c.send(&ClientRequest::Update {
            component: name.to_string(),
        })? {
            ComponentUpdateResult::AtLatestVersion => {
                // Shouldn't happen unless we raced with another client
                eprintln!(
                    "warning: Expected update for {}, raced with a different client?",
                    name
                );
                continue;
            }
            ComponentUpdateResult::Updated {
                previous: _,
                interrupted,
                new,
            } => {
                if let Some(i) = interrupted {
                    eprintln!(
                        "warning: Continued from previous interrupted update: {}",
                        format_version(&i)
                    );
                }
                println!("Updated {}: {}", name, format_version(&new));
            }
        }
        updated = true;
    }
    if !updated {
        println!("No update available for any component.");
    }
    Ok(())
}
