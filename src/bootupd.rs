/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{bail, Context, Result};
use gio::NONE_CANCELLABLE;
use nix;
use openat_ext::OpenatDirExt;
use serde_json;
use std::collections::{BTreeMap, HashSet};
use std::io::prelude::*;
use std::path::Path;
use structopt::StructOpt;

mod filetree;
use filetree::*;
mod model;
use model::*;
mod ostreeutil;
mod sha512string;
mod util;

/// Stored in /boot to describe our state; think of it like
/// a tiny rpm/dpkg database.
pub(crate) const STATEFILE_PATH: &'static str = "boot/rpmostree-bootupd-state.json";
/// The path to the ESP mount
#[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
pub(crate) const EFI_MOUNT: &'static str = "boot/efi";

#[derive(Debug, StructOpt)]
#[structopt(rename_all = "kebab-case")]
struct UpdateOptions {
    /// The system root
    #[structopt(default_value = "/", long)]
    sysroot: String,

    // Perform an update even if there is no state transition
    #[structopt(long)]
    force: bool,

    /// The destination ESP mount point
    #[structopt(
        default_value = "/usr/share/rpm-ostree/bootupdate-transition.json",
        long
    )]
    state_transition_file: String,
}

#[derive(Debug, StructOpt)]
#[structopt(rename_all = "kebab-case")]
struct StatusOptions {
    /// System root
    #[structopt(default_value = "/", long)]
    sysroot: String,

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
    /// Install data into the EFI System Partition
    Install {
        /// Physical root mountpoint
        #[structopt(long)]
        sysroot: String,
    },
    /// Start tracking current data found in the EFI System Partition
    Adopt {
        /// Physical root mountpoint
        #[structopt(long)]
        sysroot: String,
    },
    /// Update the EFI System Partition
    Update(UpdateOptions),
    Status(StatusOptions),
}

fn running_in_test_suite() -> bool {
    !nix::unistd::getuid().is_root()
}

#[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
pub(crate) fn validate_esp<P: AsRef<Path>>(mnt: P) -> Result<()> {
    if running_in_test_suite() {
        return Ok(());
    }
    let mnt = mnt.as_ref();
    let stat = nix::sys::statfs::statfs(mnt)?;
    let fstype = stat.filesystem_type();
    if fstype != nix::sys::statfs::MSDOS_SUPER_MAGIC {
        bail!(
            "Mount {} is not a msdos filesystem, but is {:?}",
            mnt.display(),
            fstype
        );
    };
    Ok(())
}

pub(crate) fn install(sysroot_path: &str) -> Result<()> {
    let sysroot = ostree::Sysroot::new(Some(&gio::File::new_for_path(sysroot_path)));
    sysroot.load(NONE_CANCELLABLE).context("loading sysroot")?;

    let _commit = ostreeutil::find_deployed_commit(sysroot_path)?;

    let statepath = Path::new(sysroot_path).join(STATEFILE_PATH);
    if statepath.exists() {
        bail!("{:?} already exists, cannot re-install", statepath);
    }

    let bootefi = Path::new(sysroot_path).join(EFI_MOUNT);
    validate_esp(&bootefi)?;

    Ok(())
}

fn update_component_filesystem_at(
    _component: &Component,
    src: &openat::Dir,
    dest: &openat::Dir,
    update: &ComponentUpdateAvailable,
) -> Result<()> {
    let diff = update.diff.as_ref().expect("diff");

    // FIXME change this to only be true if we adopted
    let opts = ApplyUpdateOptions {
        skip_removals: true,
        ..Default::default()
    };
    filetree::apply_diff(src, dest, diff, Some(&opts))?;

    Ok(())
}

fn update(opts: &UpdateOptions) -> Result<()> {
    let status = compute_status(&opts.sysroot)?;
    let sysroot_dir = openat::Dir::open(opts.sysroot.as_str())?;

    for component in status.components.iter() {
        if let Some(update) = component.update.as_ref() {
            match update {
                ComponentUpdate::LatestUpdateInstalled => {
                    println!("{:?}: At the latest version", component.ctype);
                }
                ComponentUpdate::Available(update) => match &component.ctype {
                    // Yeah we need to have components be a trait with methods like update()
                    ComponentType::EFI => {
                        let src = sysroot_dir.sub_dir("usr/lib/ostree-boot/efi")?;
                        let dest = sysroot_dir.sub_dir("boot/efi")?;
                        update_component_filesystem_at(component, &src, &dest, update)?;
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
    let f = {
        let f = sysroot_dir.new_unnamed_file(0o644)?;
        let mut buff = std::io::BufWriter::new(f);
        serde_json::to_writer(&mut buff, state)?;
        buff.flush()?;
        buff.into_inner()?
    };
    let dest = Path::new(STATEFILE_PATH);
    let dest_tmp_name = format!("{}.tmp", dest.file_name().unwrap().to_str().unwrap());
    let dest_tmp = dest.with_file_name(dest_tmp_name);
    sysroot_dir.link_file_at(&f, &dest_tmp)?;
    f.sync_all()?;
    sysroot_dir.local_rename(&dest_tmp, dest)?;
    Ok(())
}

fn adopt(sysroot_path: &str) -> Result<()> {
    let status = compute_status(sysroot_path)?;
    let mut new_saved_components = BTreeMap::new();
    let mut adopted = std::collections::BTreeSet::new();
    for component in status.components {
        let installed = match component.installed {
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
                saved,
                drift,
            } => {
                if drift {
                    eprintln!("Warning: Skipping drifted component: {:?}", component.ctype);
                }
                new_saved_components.insert(component.ctype.clone(), saved.clone());
                continue;
            }
        };
        let saved = SavedComponent {
            adopted: true,
            digest: disk.digest,
            timestamp: disk.timestamp,
            pending: None,
        };
        new_saved_components.insert(component.ctype, saved);
    }
    if adopted.len() == 0 {
        println!("Nothing to do.");
        return Ok(());
    }

    let new_saved_state = SavedState {
        components: new_saved_components,
    };
    let sysroot_dir = openat::Dir::open(sysroot_path)?;
    update_state(&sysroot_dir, &new_saved_state)?;
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
    let espdir = sysroot_dir.sub_dir("boot/efi")?;
    let content = InstalledContent::from_file_tree(FileTree::new_from_dir(&espdir)?);
    let digest = content.digest.clone();
    let saved_state = saved_components
        .map(|s| s.components.get(&ComponentType::EFI))
        .flatten();
    let installed_state = if let Some(saved) = saved_state {
        let drift = saved.digest != content.digest;
        ComponentInstalled::Tracked {
            disk: content,
            saved: saved.clone(),
            drift: drift,
        }
    } else {
        ComponentInstalled::Unknown(content)
    };
    let installed_tree = installed_state
        .get_disk_content()
        .filesystem
        .as_ref()
        .unwrap();

    let update_esp = efi_content_version_from_ostree(sysroot_path)?;
    let update_esp_tree = update_esp.content.filesystem.as_ref().unwrap();
    let update = if update_esp.content.digest == digest {
        ComponentUpdate::LatestUpdateInstalled
    } else {
        let diff = installed_tree.diff(update_esp_tree)?;
        ComponentUpdate::Available(ComponentUpdateAvailable {
            update: update_esp,
            diff: Some(diff),
        })
    };

    Ok(Component {
        ctype: ComponentType::EFI,
        installed: ComponentState::Found(installed_state),
        pending: saved_state.map(|x| x.pending.clone()).flatten(),
        update: Some(update),
    })
}

fn compute_status(sysroot_path: &str) -> Result<Status> {
    let sysroot_dir = openat::Dir::open(sysroot_path)?;

    let saved_state = if let Some(statusf) = sysroot_dir.open_file_optional(STATEFILE_PATH)? {
        let bufr = std::io::BufReader::new(statusf);
        let saved_state: SavedState = serde_json::from_reader(bufr)?;
        Some(saved_state)
    } else {
        None
    };

    let mut components = Vec::new();

    #[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
    components.push(compute_status_efi(&sysroot_path, saved_state.as_ref())?);

    #[cfg(target_arch = "x86_64")]
    {
        components.push(Component {
            ctype: ComponentType::BIOS,
            installed: ComponentState::NotImplemented,
            pending: None,
            update: None,
        });
    }
    Ok(Status {
        supported_architecture: components.len() > 0,
        components: components,
    })
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
                println!("  Installed: {}", disk.digest);
                if saved.adopted {
                    println!("    Adopted: true")
                }
            } else {
                println!("  Installed; warning: drift detected");
                println!("    Recorded: {}", saved.digest);
                println!("    Actual: {}", disk.digest);
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
                println!("  Update: Available: {}", ts_str);
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
    let status = compute_status(&opts.sysroot)?;
    if opts.json {
        serde_json::to_writer_pretty(std::io::stdout(), &status)?;
    } else {
        if !status.supported_architecture {
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
            for component in &status.components {
                if let Some(specified_components) = specified_components.as_ref() {
                    if !specified_components.contains(&component.ctype) {
                        continue;
                    }
                }
                print_component(component);
            }
        }
    }
    Ok(())
}

/// Main entrypoint
#[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
pub fn boot_update_main(args: &Vec<String>) -> Result<()> {
    let opt = Opt::from_iter(args.iter());
    match opt {
        Opt::Install { sysroot } => install(&sysroot).context("boot data installation failed")?,
        Opt::Adopt { sysroot } => adopt(&sysroot)?,
        Opt::Update(ref opts) => update(opts).context("boot data update failed")?,
        Opt::Status(ref opts) => status(opts).context("status failed")?,
    };
    Ok(())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "arm")))]
pub fn boot_update_main(args: &Vec<String>) -> Result<()> {
    bail!("This command is only supported on x86_64")
}
