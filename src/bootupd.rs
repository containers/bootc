/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use gio::NONE_CANCELLABLE;
use openat_ext::OpenatDirExt;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::prelude::*;
use std::path::Path;
use structopt::StructOpt;

#[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
mod efi;
mod filetree;
use filetree::*;
mod model;
use model::*;
mod ostreeutil;
mod sha512string;
mod util;

pub(crate) const BOOTUPD_SOCKET: &str = "/run/bootupd.sock";
/// Stored in /boot to describe our state; think of it like
/// a tiny rpm/dpkg database.  It's stored in /boot
pub(crate) const STATEFILE_DIR: &str = "boot";
pub(crate) const STATEFILE_NAME: &str = "bootupd-state.json";
pub(crate) const WRITE_LOCK_PATH: &str = "run/bootupd-lock";

/// Where rpm-ostree rewrites data that goes in /boot
pub(crate) const OSTREE_BOOT_DATA: &str = "usr/lib/ostree-boot";

#[derive(Debug, StructOpt)]
#[structopt(rename_all = "kebab-case")]
struct UpdateOptions {
    // Perform an update even if there is no state transition
    #[structopt(long)]
    force: bool,

    /// The destination ESP mount point
    #[structopt(default_value = "/usr/share/bootd-transitions.json", long)]
    state_transition_file: String,

    /// Only upgrade these components
    components: Vec<String>,
}

#[derive(Debug, StructOpt)]
#[structopt(rename_all = "kebab-case")]
struct StatusOptions {
    #[structopt(long = "component")]
    components: Option<Vec<String>>,

    // Output JSON
    #[structopt(long)]
    json: bool,
}

#[derive(Debug, StructOpt)]
#[structopt(name = "boot-update")]
#[structopt(rename_all = "kebab-case")]
enum Opt {
    /// Install data from available components into a disk image
    Install {
        /// Physical root mountpoint
        sysroot: String,
    },
    /// Start tracking current data found in available components
    Adopt,
    /// Update available components
    Update(UpdateOptions),
    Status(StatusOptions),
    Daemon,
    PingDaemon,
}

pub(crate) fn install(sysroot_path: &str) -> Result<()> {
    let sysroot = ostree::Sysroot::new(Some(&gio::File::new_for_path(sysroot_path)));
    sysroot.load(NONE_CANCELLABLE).context("loading sysroot")?;

    let _commit = ostreeutil::find_deployed_commit(sysroot_path)?;

    let statepath = Path::new(sysroot_path)
        .join(STATEFILE_DIR)
        .join(STATEFILE_NAME);
    if statepath.exists() {
        bail!("{:?} already exists, cannot re-install", statepath);
    }

    let bootefi = Path::new(sysroot_path).join(efi::MOUNT_PATH);
    efi::validate_esp(&bootefi)?;

    Ok(())
}

fn update_component_filesystem_at(
    saved: &SavedComponent,
    src: &openat::Dir,
    dest: &openat::Dir,
    update: &ComponentUpdateAvailable,
) -> Result<SavedComponent> {
    let diff = update.diff.as_ref().expect("diff");

    // For components which were adopted, we don't prune files that we don't know
    // about.
    let opts = ApplyUpdateOptions {
        skip_removals: saved.adopted,
        ..Default::default()
    };
    filetree::apply_diff(src, dest, diff, Some(&opts))?;

    Ok(SavedComponent {
        adopted: saved.adopted,
        filesystem: update.update.content.filesystem.clone(),
        digest: update.update.content.digest.clone(),
        timestamp: update.update.content_timestamp,
        pending: None,
    })
}

fn parse_componentlist(components: &[String]) -> Result<Option<BTreeSet<ComponentType>>> {
    if components.is_empty() {
        return Ok(None);
    }
    let r: std::result::Result<BTreeSet<_>, _> = components
        .iter()
        .map(|c| serde_plain::from_str(c))
        .collect();
    Ok(Some(r?))
}

fn acquire_write_lock<P: AsRef<Path>>(sysroot: P) -> Result<std::fs::File> {
    let sysroot = sysroot.as_ref();
    let mut lockf = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(sysroot.join(WRITE_LOCK_PATH))?;
    lockf.lock_exclusive()?;
    writeln!(&mut lockf, "Acquired by pid {}", nix::unistd::getpid())?;
    Ok(lockf)
}

fn update(opts: &UpdateOptions) -> Result<()> {
    let sysroot = "/";
    let _lockf = acquire_write_lock(sysroot)?;
    let (status, mut new_saved_state) = compute_status(sysroot).context("computing status")?;
    let sysroot_dir =
        openat::Dir::open(sysroot).with_context(|| format!("opening sysroot {}", sysroot))?;

    let specified_components = parse_componentlist(&opts.components)?;
    for (ctype, component) in status.components.iter() {
        let is_specified = if let Some(specified) = specified_components.as_ref() {
            if !specified.contains(ctype) {
                continue;
            }
            true
        } else {
            false
        };
        let saved = match &component.installed {
            ComponentState::NotImplemented => {
                if is_specified {
                    bail!("Component {:?} is not implemented", &ctype);
                } else {
                    continue;
                }
            }
            ComponentState::NotInstalled => {
                if is_specified {
                    bail!("Component {:?} is not installed", &ctype);
                } else {
                    continue;
                }
            }
            ComponentState::Found(installed) => match installed {
                ComponentInstalled::Unknown(_) => {
                    if is_specified {
                        bail!(
                            "Component {:?} is not tracked and must be adopted before update",
                            ctype
                        );
                    } else {
                        println!(
                            "Skipping component {:?} which is found but not adopted",
                            ctype
                        );
                        continue;
                    }
                }
                ComponentInstalled::Tracked { disk: _, saved, .. } => saved,
            },
        };
        // If we get here, there must be saved state
        let new_saved_state = new_saved_state.as_mut().expect("saved state");

        if let Some(update) = component.update.as_ref() {
            match update {
                ComponentUpdate::LatestUpdateInstalled => {
                    println!("{:?}: At the latest version", component.ctype);
                }
                ComponentUpdate::Available(update) => match &component.ctype {
                    // Yeah we need to have components be a trait with methods like update()
                    ComponentType::EFI => {
                        let src = sysroot_dir
                            .sub_dir(&Path::new(OSTREE_BOOT_DATA).join("efi"))
                            .context("opening ostree boot data")?;
                        let dest = sysroot_dir
                            .sub_dir(efi::MOUNT_PATH)
                            .context(efi::MOUNT_PATH)?;
                        let updated_component =
                            update_component_filesystem_at(saved, &src, &dest, update)
                                .with_context(|| format!("updating component {:?}", ctype))?;
                        let updated_digest = updated_component.digest.clone();
                        new_saved_state
                            .components
                            .insert(ComponentType::EFI, updated_component);
                        update_state(&sysroot_dir, new_saved_state)?;
                        println!("{:?}: Updated to digest={}", ctype, updated_digest);
                    }
                    ctype => {
                        panic!("Unhandled update for component {:?}", ctype);
                    }
                },
            }
        } else {
            println!("{:?}: No updates available", component.ctype);
        };
    }
    Ok(())
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

fn adopt() -> Result<()> {
    let sysroot_path = "/";
    let _lockf = acquire_write_lock(sysroot_path)?;
    let (status, saved_state) = compute_status(sysroot_path)?;
    let mut adopted = std::collections::BTreeSet::new();
    let mut saved_state = saved_state.unwrap_or_else(|| SavedState {
        components: BTreeMap::new(),
    });
    for (ctype, component) in status.components.iter() {
        let installed = match &component.installed {
            ComponentState::NotInstalled => continue,
            ComponentState::NotImplemented => continue,
            ComponentState::Found(installed) => installed,
        };
        let disk = match installed {
            ComponentInstalled::Unknown(state) => {
                println!("Adopting: {:?}", component.ctype);
                adopted.insert(component.ctype.clone());
                state
            }
            ComponentInstalled::Tracked {
                disk: _,
                saved: _,
                drift,
            } => {
                if *drift {
                    eprintln!("Warning: Skipping drifted component: {:?}", ctype);
                }
                continue;
            }
        };
        let saved = SavedComponent {
            adopted: true,
            digest: disk.digest.clone(),
            filesystem: disk.filesystem.clone(),
            timestamp: disk.timestamp,
            pending: None,
        };
        saved_state
            .components
            .insert(component.ctype.clone(), saved);
    }
    if adopted.is_empty() {
        println!("Nothing to do.");
        return Ok(());
    }
    // Must have saved state if we get here
    let sysroot_dir = openat::Dir::open(sysroot_path)?;
    update_state(&sysroot_dir, &saved_state)?;
    Ok(())
}

fn timestamp_and_commit_from_sysroot(
    sysroot_path: &str,
) -> Result<(chrono::naive::NaiveDateTime, String)> {
    // Until we have list_deployments
    assert!(sysroot_path == "/");
    let ostree_sysroot = ostree::Sysroot::new(Some(&gio::File::new_for_path(sysroot_path)));
    ostree_sysroot
        .load(NONE_CANCELLABLE)
        .context("loading sysroot")?;
    let booted_deployment = if let Some(booted) = ostree_sysroot.get_booted_deployment() {
        booted
    } else {
        bail!("Not booted into an OSTree system")
    };

    let repo = ostree_sysroot.repo().expect("repo");
    let csum = booted_deployment.get_csum().expect("booted csum");
    let csum = csum.as_str();
    let (commit, _) = repo.load_commit(csum)?;
    let ts = ostree::commit_get_timestamp(&commit);
    let ts = chrono::naive::NaiveDateTime::from_timestamp(ts as i64, 0);
    Ok((ts, csum.to_string()))
}

fn efi_content_version_from_ostree(sysroot_path: &str) -> Result<ContentVersion> {
    let (ts, commit) = if let Some(timestamp_str) = util::getenv_utf8("BOOT_UPDATE_TEST_TIMESTAMP")?
    {
        let ts = chrono::NaiveDateTime::parse_from_str(&timestamp_str, "%+")?;
        (ts, None)
    } else {
        let (ts, commit) = timestamp_and_commit_from_sysroot(sysroot_path)?;
        (ts, Some(commit))
    };
    let sysroot_dir = openat::Dir::open(sysroot_path)?;
    let update_esp_dir = sysroot_dir.sub_dir("usr/lib/ostree-boot/efi")?;
    let ft = FileTree::new_from_dir(&update_esp_dir)?;
    Ok(ContentVersion {
        content_timestamp: ts,
        content: InstalledContent::from_file_tree(ft),
        ostree_commit: commit,
    })
}

#[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
fn compute_status_efi(
    sysroot_path: &str,
    saved_components: Option<&SavedState>,
) -> Result<Component> {
    let sysroot_dir = openat::Dir::open(sysroot_path)?;
    let espdir = sysroot_dir
        .sub_dir(efi::MOUNT_PATH)
        .context(efi::MOUNT_PATH)?;
    let esptree = FileTree::new_from_dir(&espdir).context("computing filetree for efi")?;
    let saved = saved_components
        .map(|s| s.components.get(&ComponentType::EFI))
        .flatten();
    let saved = if let Some(saved) = saved {
        saved
    } else {
        return Ok(Component {
            ctype: ComponentType::EFI,
            installed: ComponentState::Found(ComponentInstalled::Unknown(
                InstalledContent::from_file_tree(esptree),
            )),
            pending: None,
            update: None,
        });
    };
    let fsdiff = if let Some(saved_filesystem) = saved.filesystem.as_ref() {
        Some(esptree.diff(&saved_filesystem)?)
    } else {
        None
    };
    let fsdiff = fsdiff.as_ref();
    let (installed, installed_digest) = {
        let content = InstalledContent::from_file_tree(esptree);
        let drift = if let Some(fsdiff) = fsdiff {
            if saved.adopted {
                !fsdiff.is_only_removals()
            } else {
                fsdiff.count() > 0
            }
        } else {
            // TODO detect state outside of filesystem tree
            false
        };
        (
            ComponentInstalled::Tracked {
                disk: content,
                saved: saved.clone(),
                drift,
            },
            saved.digest.clone(),
        )
    };
    let installed_tree = installed.get_disk_content().filesystem.as_ref().unwrap();

    let update_esp = efi_content_version_from_ostree(sysroot_path)?;
    let update_esp_tree = update_esp.content.filesystem.as_ref().unwrap();
    let update = if !saved.adopted && update_esp.content.digest == installed_digest {
        ComponentUpdate::LatestUpdateInstalled
    } else {
        let diff = installed_tree.diff(update_esp_tree)?;
        if saved.adopted && diff.is_only_removals() {
            ComponentUpdate::LatestUpdateInstalled
        } else {
            ComponentUpdate::Available(Box::new(ComponentUpdateAvailable {
                update: update_esp,
                diff: Some(diff),
            }))
        }
    };

    Ok(Component {
        ctype: ComponentType::EFI,
        installed: ComponentState::Found(installed),
        pending: saved.pending.clone(),
        update: Some(update),
    })
}

fn compute_status(sysroot_path: &str) -> Result<(Status, Option<SavedState>)> {
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

    let mut components = BTreeMap::new();

    #[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
    components.insert(
        ComponentType::EFI,
        compute_status_efi(&sysroot_path, saved_state.as_ref())?,
    );

    #[cfg(target_arch = "x86_64")]
    {
        components.insert(
            ComponentType::BIOS,
            Component {
                ctype: ComponentType::BIOS,
                installed: ComponentState::NotImplemented,
                pending: None,
                update: None,
            },
        );
    }
    Ok((
        Status {
            supported_architecture: !components.is_empty(),
            components,
        },
        saved_state,
    ))
}

fn print_component(component: &Component) {
    let name = serde_plain::to_string(&component.ctype).expect("serde");
    println!("Component {}", name);
    let installed = match &component.installed {
        ComponentState::NotInstalled => {
            println!("  Not installed.");
            return;
        }
        ComponentState::NotImplemented => {
            println!("  Not implemented.");
            return;
        }
        ComponentState::Found(installed) => installed,
    };
    match installed {
        ComponentInstalled::Unknown(disk) => {
            println!("  Unmanaged: digest={}", disk.digest);
        }
        ComponentInstalled::Tracked { disk, saved, drift } => {
            if !*drift {
                println!("  Installed: {}", saved.digest);
            } else {
                println!("  Installed; warning: drift detected");
                println!("      Recorded: {}", saved.digest);
                println!("      Actual: {}", disk.digest);
            }
            if saved.adopted {
                println!("    Adopted: true")
            }
        }
    }
    if let Some(update) = component.update.as_ref() {
        match update {
            ComponentUpdate::LatestUpdateInstalled => {
                println!("  Update: At latest version");
            }
            ComponentUpdate::Available(update) => {
                let ts_str = update
                    .update
                    .content_timestamp
                    .format("%Y-%m-%dT%H:%M:%S+00:00");
                println!("  Update: Available");
                println!("    Timestamp: {}", ts_str);
                println!("    Digest: {}", update.update.content.digest);
                if let Some(diff) = &update.diff {
                    println!(
                        "    Diff: changed={} added={} removed={}",
                        diff.changes.len(),
                        diff.additions.len(),
                        diff.removals.len()
                    );
                }
            }
        }
    }
}

fn status(opts: &StatusOptions) -> Result<()> {
    let (status, _) = compute_status("/")?;
    if opts.json {
        serde_json::to_writer_pretty(std::io::stdout(), &status)?;
    } else if !status.supported_architecture {
        eprintln!("This architecture is not supported.")
    } else {
        let specified_components = if let Some(components) = &opts.components {
            let r: std::result::Result<HashSet<ComponentType>, _> = components
                .iter()
                .map(|c| serde_plain::from_str(c))
                .collect();
            Some(r?)
        } else {
            None
        };
        for (ctype, component) in status.components.iter() {
            if let Some(specified_components) = specified_components.as_ref() {
                if !specified_components.contains(ctype) {
                    continue;
                }
            }
            print_component(component);
        }
    }
    Ok(())
}

fn daemon() -> Result<()> {
    use libsystemd::daemon::{self, NotifyState};
    use nix::sys::socket as nixsocket;
    use nix::sys::uio::IoVec;
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
    let mut buf = [0u8; 1024];
    loop {
        let fd = nixsocket::accept4(srvsock_fd, nixsocket::SockFlag::SOCK_CLOEXEC)?;
        nixsocket::setsockopt(fd, nix::sys::socket::sockopt::PassCred, &true)?;
        let iov = IoVec::from_mut_slice(buf.as_mut());
        let mut cmsgspace = nix::cmsg_space!(nixsocket::UnixCredentials);
        let msg = nixsocket::recvmsg(
            fd,
            &[iov],
            Some(&mut cmsgspace),
            nixsocket::MsgFlags::MSG_CMSG_CLOEXEC,
        )?;
        let mut uid = None;
        for cmsg in msg.cmsgs() {
            if let nixsocket::ControlMessageOwned::ScmCredentials(creds) = cmsg {
                uid = Some(creds.uid());
                break;
            }
        }
        if let Some(uid) = uid {
            if uid != 0 {
                eprintln!("Dropping connection from unauthorized uid: {}", uid);
                continue;
            }
        } else {
            eprintln!("No SCM rights provided");
            continue;
        }
        dbg!(String::from_utf8_lossy(&buf[0..msg.bytes]));
    }
}

fn ping_daemon() -> Result<()> {
    use nix::sys::socket as nixsocket;
    use nix::sys::uio::IoVec;
    let sock = nixsocket::socket(
        nixsocket::AddressFamily::Unix,
        nixsocket::SockType::SeqPacket,
        nixsocket::SockFlag::SOCK_CLOEXEC,
        None,
    )?;
    let addr = nixsocket::SockAddr::new_unix(BOOTUPD_SOCKET)?;
    nixsocket::connect(sock, &addr)?;
    let creds = libc::ucred {
        pid: nix::unistd::getpid().as_raw(),
        uid: nix::unistd::getuid().as_raw(),
        gid: nix::unistd::getgid().as_raw(),
    };
    let creds = nixsocket::UnixCredentials::from(creds);
    let creds = nixsocket::ControlMessage::ScmCredentials(&creds);
    nixsocket::sendmsg(
        sock,
        &[IoVec::from_slice(b"ping")],
        &[creds],
        nixsocket::MsgFlags::MSG_CMSG_CLOEXEC,
        None,
    )?;

    Ok(())
}

/// Main entrypoint
#[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
pub fn boot_update_main(args: &[String]) -> Result<()> {
    let opt = Opt::from_iter(args.iter());
    match opt {
        Opt::Install { sysroot } => install(&sysroot).context("boot data installation failed")?,
        Opt::Adopt => adopt()?,
        Opt::Update(ref opts) => update(opts)?,
        Opt::Status(ref opts) => status(opts)?,
        Opt::Daemon => daemon()?,
        Opt::PingDaemon => ping_daemon()?,
    };
    Ok(())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "arm")))]
pub fn boot_update_main(args: &Vec<String>) -> Result<()> {
    bail!("This command is only supported on x86_64")
}
