/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

#![deny(unused_must_use)]

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use nix::sys::socket as nixsocket;
use openat_ext::OpenatDirExt;
use serde::{Deserialize, Serialize};
use std::fmt::Write as WriteFmt;
use std::io::prelude::*;
use std::path::Path;
use structopt::StructOpt;

// #[cfg(any(target_arch = "x86_64"))]
// mod bios;
mod component;
#[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
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
struct UpdateOptions {
    // Perform an update even if there is no state transition
    #[structopt(long)]
    force: bool,

    /// The destination ESP mount point
    #[structopt(default_value = "/usr/share/bootd-transitions.json", long)]
    state_transition_file: String,
}

#[derive(Debug, Serialize, Deserialize, StructOpt)]
#[structopt(rename_all = "kebab-case")]
struct StatusOptions {
    components: Vec<String>,

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
    /// Update available components
    Update(UpdateOptions),
    /// Print the current state
    Status(StatusOptions),
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

    #[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
    components.push(Box::new(efi::EFI::new()));

    // #[cfg(target_arch = "x86_64")]
    // components.push(Box::new(bios::BIOS::new()));

    components
}

pub(crate) fn generate_update_metadata(sysroot_path: &str) -> Result<()> {
    for component in get_components() {
        let _ = component.generate_update_metadata(sysroot_path)?;
    }

    Ok(())
}

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

fn update(_opts: &UpdateOptions) -> Result<String> {
    let sysroot = openat::Dir::open("/")?;
    let _lock = acquire_write_lock("/")?;
    let mut r = String::new();
    let mut state = get_saved_state("/")?.unwrap_or_else(|| SavedState {
        ..Default::default()
    });
    for component in get_components() {
        let installed = if let Some(i) = state.installed.get(component.name()) {
            i
        } else {
            writeln!(r, "Component {} is not installed", component.name())?;
            continue;
        };
        let pending = component.query_update()?;
        let update = match pending.as_ref() {
            Some(p) if !p.compare(&installed.meta) => Some(p),
            _ => None,
        };
        if let Some(update) = update {
            // FIXME make this more transactional by recording the fact that
            // we're starting an update at least so we can detect if we
            // were interrupted.
            let newinst = component
                .run_update(&installed)
                .with_context(|| format!("Failed to update {}", component.name()))?;
            writeln!(
                r,
                "Updated {}: {}",
                component.name(),
                format_version(&update)
            )?;
            state.installed.insert(component.name().into(), newinst);
            update_state(&sysroot, &state)?;
        } else {
            writeln!(
                r,
                "No update available for {}: {:?}",
                component.name(),
                installed
            )?;
        }
    }
    Ok(r)
}

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

fn format_version(meta: &ContentMetadata) -> String {
    if let Some(version) = meta.version.as_ref() {
        version.into()
    } else {
        meta.timestamp.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    }
}

fn print_component(
    component: &dyn Component,
    installed: &ContentMetadata,
    r: &mut String,
) -> Result<()> {
    let name = component.name();
    writeln!(r, "Component {}", name)?;
    writeln!(r, "  Installed: {}", format_version(installed))?;
    let pending = component.query_update()?;
    let update = match pending.as_ref() {
        Some(p) if !p.compare(installed) => Some(p),
        _ => None,
    };
    if let Some(update) = update {
        writeln!(r, "  Update: Available: {}", format_version(&update))?;
    } else {
        writeln!(r, "  Update: At latest version")?;
    }

    Ok(())
}

fn status(opts: &StatusOptions) -> Result<String> {
    let state = get_saved_state("/")?;
    if opts.json {
        let r = serde_json::to_string(&state)?;
        Ok(r)
    } else if let Some(state) = state {
        let mut r = String::new();
        for (name, ic) in state.installed.iter() {
            let component = component::new_from_name(&name)?;
            let component = component.as_ref();
            print_component(component, &ic.meta, &mut r)?;
        }
        Ok(r)
    } else {
        Ok("No components installed.".to_string())
    }
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

        let opt = bincode::deserialize(&buf)?;
        let r = match opt {
            Opt::Update(ref opts) => {
                println!("Processing update");
                update(opts)
            }
            Opt::Status(ref opts) => {
                println!("Processing status");
                status(opts)
            }
        };
        let r = match r {
            Ok(s) => ipc::DaemonToClientReply::Success(s),
            Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
        };
        let r = bincode::serialize(&r)?;
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

pub fn backend_main(args: &[&str]) -> Result<()> {
    let opt = BackendOpt::from_iter(args.iter());
    match opt {
        BackendOpt::Install {
            src_root,
            dest_root,
        } => install(&src_root, &dest_root).context("boot data installation failed")?,
        BackendOpt::GenerateUpdateMetadata { sysroot } => {
            generate_update_metadata(&sysroot).context("generating metadata failed")?
        }
    };
    Ok(())
}

pub fn frontend_main(args: &[&str]) -> Result<()> {
    let opt = Opt::from_iter(args.iter());
    let mut c = ipc::ClientToDaemonConnection::new();
    c.connect()?;
    let r = c.send(&opt)?;
    match r {
        ipc::DaemonToClientReply::Success(buf) => {
            print!("{}", buf);
        }
        ipc::DaemonToClientReply::Failure(buf) => {
            bail!("{}", buf);
        }
    }
    c.shutdown()?;
    Ok(())
}

/// Main entrypoint
pub fn boot_update_main(args: &[String]) -> Result<()> {
    let mut args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    if let Some(&argv1) = args.get(1) {
        if argv1 == "backend" {
            args.remove(1);
            return backend_main(&args);
        } else if argv1 == "daemon" {
            return daemon();
        }
    }
    frontend_main(&args)
}
