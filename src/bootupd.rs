/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{bail, Context, Result};
use byteorder::ByteOrder;
use chrono::prelude::*;
use gio::NONE_CANCELLABLE;
use nix;
use openat_ext::OpenatDirExt;
use openssl::hash::{Hasher, MessageDigest};
use serde_derive::{Deserialize, Serialize};
use serde_json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::prelude::*;
use std::path::Path;
use structopt::StructOpt;

mod sha512string;
use sha512string::SHA512String;

/// Metadata for a single file
#[derive(Serialize, Deserialize, Clone, Debug, Hash, Ord, PartialOrd, PartialEq, Eq)]
pub(crate) enum ComponentType {
    #[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
    EFI,
    #[cfg(target_arch = "x86_64")]
    BIOS,
}

/// Metadata for a single file
#[derive(Serialize, Debug, Hash, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct FileMetadata {
    /// File size in bytes
    pub(crate) size: u64,
    /// Content checksum; chose SHA-512 because there are not a lot of files here
    /// and it's ok if the checksum is large.
    pub(crate) sha512: SHA512String,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct FileTree {
    pub(crate) timestamp: NaiveDateTime,
    pub(crate) children: BTreeMap<String, FileMetadata>,
}

/// Describes data that is at the block level or the filesystem level.
#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct InstalledContent {
    /// sha512 of the state of the content
    pub(crate) digest: SHA512String,
    pub(crate) timestamp: NaiveDateTime,
    pub(crate) filesystem: Option<Box<FileTree>>,
}

/// A versioned description of something we can update,
/// whether that is a BIOS MBR or an ESP
#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct ContentVersion {
    pub(crate) content_timestamp: NaiveDateTime,
    pub(crate) content: InstalledContent,
    pub(crate) ostree_commit: Option<String>,
}

/// The state of a particular managed component as found on disk
#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ComponentInstalled {
    Unknown(InstalledContent),
    Tracked {
        disk: InstalledContent,
        saved: SavedComponent,
        drift: bool,
    },
}

/// The state of a particular managed component as found on disk
#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ComponentState {
    #[allow(dead_code)]
    NotInstalled,
    NotImplemented,
    Found(ComponentInstalled),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FileTreeDiff {
    pub(crate) additions: HashSet<String>,
    pub(crate) removals: HashSet<String>,
    pub(crate) changes: HashSet<String>,
}

/// The state of a particular managed component
#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ComponentUpdate {
    LatestUpdateInstalled,
    Available {
        update: ContentVersion,
        diff: Option<FileTreeDiff>,
    },
}

/// A component along with a possible update
#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct Component {
    pub(crate) ctype: ComponentType,
    pub(crate) installed: ComponentState,
    pub(crate) pending: Option<SavedPendingUpdate>,
    pub(crate) update: Option<ComponentUpdate>,
}

/// Our total view of the world at a point in time
#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct Status {
    pub(crate) supported_architecture: bool,
    pub(crate) components: Vec<Component>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct SavedPendingUpdate {
    /// The value of /proc/sys/kernel/random/boot_id
    pub(crate) boot_id: String,
    /// The value of /etc/machine-id from the OS trying to update
    pub(crate) machineid: String,
    /// The new version we're trying to install
    pub(crate) digest: SHA512String,
    pub(crate) timestamp: NaiveDateTime,
}

/// Will be serialized into /boot/rpmostree-bootupd-state.json
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct SavedComponent {
    pub(crate) adopted: bool,
    pub(crate) digest: SHA512String,
    pub(crate) timestamp: NaiveDateTime,
    pub(crate) pending: Option<SavedPendingUpdate>,
}

/// Will be serialized into /boot/rpmostree-bootupd-state.json
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct SavedState {
    pub(crate) components: BTreeMap<ComponentType, SavedComponent>,
}

/// Should be stored in /usr/lib/rpm-ostree/bootupdate-edge.json
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct UpgradeEdge {
    /// Set to true if we should upgrade from an unknown state
    #[serde(default)]
    pub(crate) from_unknown: bool,
    /// Upgrade from content past this timestamp
    pub(crate) from_timestamp: Option<NaiveDateTime>,
}

/// Stored in /boot to describe our state; think of it like
/// a tiny rpm/dpkg database.
pub(crate) const STATEFILE_PATH: &'static str = "boot/rpmostree-bootupd-state.json";
/// The path to the ESP mount
#[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
pub(crate) const EFI_MOUNT: &'static str = "boot/efi";

/// The prefix we apply to our temporary files.
pub(crate) const TMP_PREFIX: &'static str = ".btmp.";

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

impl SHA512String {
    fn from_hasher(hasher: &mut Hasher) -> Self {
        Self(format!(
            "sha512:{}",
            bs58::encode(hasher.finish().expect("completing hash")).into_string()
        ))
    }

    fn digest_bs58(&self) -> &str {
        self.0.splitn(2, ":").next().unwrap()
    }

    fn digest_bytes(&self) -> Vec<u8> {
        bs58::decode(self.digest_bs58())
            .into_vec()
            .expect("decoding bs58 hash")
    }
}

impl FileMetadata {
    pub(crate) fn new_from_path<P: openat::AsPath>(
        dir: &openat::Dir,
        name: P,
    ) -> Result<FileMetadata> {
        let mut r = dir.open_file(name)?;
        let meta = r.metadata()?;
        let mut hasher =
            Hasher::new(MessageDigest::sha512()).expect("openssl sha512 hasher creation failed");
        std::io::copy(&mut r, &mut hasher)?;
        let digest = SHA512String::from_hasher(&mut hasher);
        Ok(FileMetadata {
            size: meta.len(),
            sha512: digest,
        })
    }

    pub(crate) fn extend_hash(&self, hasher: &mut Hasher) {
        let mut lenbuf = [0; 8];
        byteorder::BigEndian::write_u64(&mut lenbuf, self.size);
        hasher.update(&lenbuf).unwrap();
        hasher.update(&self.sha512.digest_bytes()).unwrap();
    }
}

impl FileTree {
    // Internal helper to generate a sub-tree
    fn unsorted_from_dir(dir: &openat::Dir) -> Result<HashMap<String, FileMetadata>> {
        let mut ret = HashMap::new();
        for entry in dir.list_dir(".")? {
            let entry = entry?;
            let name = if let Some(name) = entry.file_name().to_str() {
                name
            } else {
                bail!("Invalid UTF-8 filename: {:?}", entry.file_name())
            };
            if name.starts_with(TMP_PREFIX) {
                bail!("File {} contains our temporary prefix!", name);
            }
            match dir.get_file_type(&entry)? {
                openat::SimpleType::File => {
                    let meta = FileMetadata::new_from_path(dir, name)?;
                    ret.insert(name.to_string(), meta);
                }
                openat::SimpleType::Dir => {
                    let child = dir.sub_dir(name)?;
                    for (mut k, v) in FileTree::unsorted_from_dir(&child)?.drain() {
                        k.reserve(name.len() + 1);
                        k.insert(0, '/');
                        k.insert_str(0, name);
                        ret.insert(k, v);
                    }
                }
                openat::SimpleType::Symlink => {
                    bail!("Unsupported symbolic link {:?}", entry.file_name())
                }
                openat::SimpleType::Other => {
                    bail!("Unsupported non-file/directory {:?}", entry.file_name())
                }
            }
        }
        Ok(ret)
    }

    fn digest(&self) -> SHA512String {
        let mut hasher =
            Hasher::new(MessageDigest::sha512()).expect("openssl sha512 hasher creation failed");
        for (k, v) in self.children.iter() {
            hasher.update(k.as_bytes()).unwrap();
            v.extend_hash(&mut hasher);
        }
        SHA512String::from_hasher(&mut hasher)
    }

    /// Create a FileTree from the target directory.
    fn new_from_dir(dir: &openat::Dir) -> Result<Self> {
        let mut children = BTreeMap::new();
        for (k, v) in Self::unsorted_from_dir(dir)?.drain() {
            children.insert(k, v);
        }

        let meta = dir.metadata(".")?;
        let stat = meta.stat();

        Ok(Self {
            timestamp: chrono::NaiveDateTime::from_timestamp(stat.st_mtime, 0),
            children: children,
        })
    }

    /// Determine the changes *from* self to the updated tree
    fn diff(&self, updated: &Self) -> Result<FileTreeDiff> {
        let mut additions = HashSet::new();
        let mut removals = HashSet::new();
        let mut changes = HashSet::new();

        for (k, v1) in self.children.iter() {
            if let Some(v2) = updated.children.get(k) {
                if v1 != v2 {
                    changes.insert(k.clone());
                }
            } else {
                removals.insert(k.clone());
            }
        }
        for k in updated.children.keys() {
            if let Some(_) = self.children.get(k) {
                continue;
            }
            additions.insert(k.clone());
        }
        Ok(FileTreeDiff {
            additions: additions,
            removals: removals,
            changes: changes,
        })
    }
}

impl InstalledContent {
    fn from_file_tree(ft: FileTree) -> InstalledContent {
        InstalledContent {
            digest: ft.digest(),
            timestamp: ft.timestamp,
            filesystem: Some(Box::new(ft)),
        }
    }
}

impl ComponentInstalled {
    fn get_disk_content(&self) -> &InstalledContent {
        match self {
            Self::Unknown(i) => i,
            Self::Tracked { disk, .. } => disk,
        }
    }
}

// Recursively remove all files in the directory that start with our TMP_PREFIX
fn cleanup_tmp(dir: &openat::Dir) -> Result<()> {
    for entry in dir.list_dir(".")? {
        let entry = entry?;
        let name = if let Some(name) = entry.file_name().to_str() {
            name
        } else {
            // Skip invalid UTF-8 for now, we will barf on it later though.
            continue;
        };

        match dir.get_file_type(&entry)? {
            openat::SimpleType::Dir => {
                let child = dir.sub_dir(name)?;
                cleanup_tmp(&child)?;
            }
            openat::SimpleType::File => {
                if name.starts_with(TMP_PREFIX) {
                    dir.remove_file(name)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// struct ApplyUpdateOptions {
//     skip_removals: bool,
// }

// fn apply_update_from_diff(
//     src: &openat::Dir,
//     dest: &openat::Dir,
//     diff: FileTreeDiff,
//     opts: Option<ApplyUpdateOptions>,
// ) -> Result<()> {
//     Ok(())
// }

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

mod install {
    use super::*;

    fn find_deployed_commit(sysroot_path: &str) -> Result<String> {
        // ostree_sysroot_get_deployments() isn't bound
        // https://gitlab.com/fkrull/ostree-rs/-/issues/3
        let ls = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("ls -d {}/ostree/deploy/*/deploy/*.0", sysroot_path))
            .output()?;
        if !ls.status.success() {
            bail!("failed to find deployment")
        }
        let mut lines = ls.stdout.lines();
        let deployment = if let Some(line) = lines.next() {
            let line = line?;
            let deploypath = Path::new(line.trim());
            let parts: Vec<_> = deploypath
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .splitn(2, ".0")
                .collect();
            assert!(parts.len() == 2);
            parts[0].to_string()
        } else {
            bail!("failed to find deployment");
        };
        if let Some(_) = lines.next() {
            bail!("multiple deployments found")
        }
        Ok(deployment)
    }

    pub(crate) fn install(sysroot_path: &str) -> Result<()> {
        let sysroot = ostree::Sysroot::new(Some(&gio::File::new_for_path(sysroot_path)));
        sysroot.load(NONE_CANCELLABLE).context("loading sysroot")?;

        let _commit = find_deployed_commit(sysroot_path)?;

        let statepath = Path::new(sysroot_path).join(STATEFILE_PATH);
        if statepath.exists() {
            bail!("{:?} already exists, cannot re-install", statepath);
        }

        let bootefi = Path::new(sysroot_path).join(EFI_MOUNT);
        validate_esp(&bootefi)?;

        Ok(())
    }
}

fn update(opts: &UpdateOptions) -> Result<()> {
    let esp = Path::new(&opts.sysroot).join(EFI_MOUNT);
    validate_esp(&esp)?;
    let esp_dir = openat::Dir::open(&esp)?;

    // First remove any temporary files
    cleanup_tmp(&esp_dir)?;

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
    let f = {
        let f = sysroot_dir.new_unnamed_file(0o644)?;
        let mut buff = std::io::BufWriter::new(f);
        serde_json::to_writer(&mut buff, &new_saved_state)?;
        buff.flush()?;
        buff.into_inner()?
    };
    let dest = Path::new(STATEFILE_PATH);
    let dest_tmp_name = format!("{}.tmp", dest.file_name().unwrap().to_str().unwrap());
    let dest_tmp = dest.with_file_name(dest_tmp_name);
    sysroot_dir.link_file_at(&f, &dest_tmp)?;
    sysroot_dir.local_rename(&dest_tmp, dest)?;
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

/// Parse an environment variable as UTF-8
fn getenv_utf8(n: &str) -> Result<Option<String>> {
    if let Some(v) = std::env::var_os(n) {
        Ok(Some(
            v.to_str()
                .ok_or_else(|| anyhow::anyhow!("{} is invalid UTF-8", n))?
                .to_string(),
        ))
    } else {
        Ok(None)
    }
}

fn efi_content_version_from_ostree(sysroot_path: &str) -> Result<ContentVersion> {
    let (ts, commit) = if let Some(timestamp_str) = getenv_utf8("BOOT_UPDATE_TEST_TIMESTAMP")? {
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
        dbg!(&saved);
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
        ComponentUpdate::Available {
            update: update_esp,
            diff: Some(diff),
        }
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
            ComponentUpdate::Available { update, diff } => {
                let ts_str = update.content_timestamp.format("%Y-%m-%dT%H:%M:%S+00:00");
                println!("  Update: Available: {}", ts_str);
                if let Some(diff) = diff {
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
        Opt::Install { sysroot } => {
            install::install(&sysroot).context("boot data installation failed")?
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run_diff(a: &openat::Dir, b: &openat::Dir) -> Result<FileTreeDiff> {
        let ta = FileTree::new_from_dir(&a)?;
        let tb = FileTree::new_from_dir(&b)?;
        let diff = ta.diff(&tb)?;
        Ok(diff)
    }

    #[test]
    fn test_diff() -> Result<()> {
        let tmpd = tempfile::tempdir()?;
        let p = tmpd.path();
        let a = p.join("a");
        let b = p.join("b");
        std::fs::create_dir(&a)?;
        std::fs::create_dir(&b)?;
        let a = openat::Dir::open(&a)?;
        let b = openat::Dir::open(&b)?;
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 0);
        assert_eq!(diff.changes.len(), 0);
        a.create_dir("foo", 0o755)?;
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 0);
        assert_eq!(diff.changes.len(), 0);
        {
            let mut bar = a.write_file("foo/bar", 0o644)?;
            bar.write("foobarcontents".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 1);
        assert_eq!(diff.changes.len(), 0);
        b.create_dir("foo", 0o755)?;
        {
            let mut bar = b.write_file("foo/bar", 0o644)?;
            bar.write("foobarcontents".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 0);
        assert_eq!(diff.changes.len(), 0);
        {
            let mut bar2 = b.write_file("foo/bar", 0o644)?;
            bar2.write("foobarcontents2".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 0);
        assert_eq!(diff.changes.len(), 1);
        Ok(())
    }
}
