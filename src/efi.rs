/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use std::cell::RefCell;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use fn_error_context::context;
use openat_ext::OpenatDirExt;
use walkdir::WalkDir;
use widestring::U16CString;

use crate::filetree;
use crate::model::*;
use crate::ostreeutil;
use crate::util::CommandRunExt;
use crate::{component::*, packagesystem};

/// Well-known paths to the ESP that may have been mounted external to us.
pub(crate) const ESP_MOUNTS: &[&str] = &["boot/efi", "efi", "boot"];

/// The binary to change EFI boot ordering
const EFIBOOTMGR: &str = "efibootmgr";
#[cfg(target_arch = "aarch64")]
pub(crate) const SHIM: &str = "shimaa64.efi";

#[cfg(target_arch = "x86_64")]
pub(crate) const SHIM: &str = "shimx64.efi";

/// The ESP partition label on Fedora CoreOS derivatives
pub(crate) const COREOS_ESP_PART_LABEL: &str = "EFI-SYSTEM";
pub(crate) const ANACONDA_ESP_PART_LABEL: &str = "EFI\\x20System\\x20Partition";

/// Systemd boot loader info EFI variable names
const LOADER_INFO_VAR_STR: &str = "LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
const STUB_INFO_VAR_STR: &str = "StubInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

/// Return `true` if the system is booted via EFI
pub(crate) fn is_efi_booted() -> Result<bool> {
    Path::new("/sys/firmware/efi")
        .try_exists()
        .map_err(Into::into)
}

#[derive(Default)]
pub(crate) struct Efi {
    mountpoint: RefCell<Option<PathBuf>>,
}

impl Efi {
    fn esp_path(&self) -> Result<PathBuf> {
        self.ensure_mounted_esp(Path::new("/"))
            .map(|v| v.join("EFI"))
    }

    fn open_esp_optional(&self) -> Result<Option<openat::Dir>> {
        let sysroot = openat::Dir::open("/")?;
        let esp = sysroot.sub_dir_optional(&self.esp_path()?)?;
        Ok(esp)
    }

    fn open_esp(&self) -> Result<openat::Dir> {
        self.ensure_mounted_esp(Path::new("/"))?;
        let sysroot = openat::Dir::open("/")?;
        let esp = sysroot.sub_dir(&self.esp_path()?)?;
        Ok(esp)
    }

    pub(crate) fn ensure_mounted_esp(&self, root: &Path) -> Result<PathBuf> {
        let mut mountpoint = self.mountpoint.borrow_mut();
        if let Some(mountpoint) = mountpoint.as_deref() {
            return Ok(mountpoint.to_owned());
        }
        for &mnt in ESP_MOUNTS {
            let mnt = root.join(mnt);
            if !mnt.exists() {
                continue;
            }
            let st = nix::sys::statfs::statfs(&mnt)
                .with_context(|| format!("statfs failed for {mnt:?}"))?;
            if st.filesystem_type() != nix::sys::statfs::MSDOS_SUPER_MAGIC {
                continue;
            }
            log::debug!("Reusing existing {mnt:?}");
            return Ok(mnt);
        }

        let esp_devices = [COREOS_ESP_PART_LABEL, ANACONDA_ESP_PART_LABEL]
            .into_iter()
            .map(|p| Path::new("/dev/disk/by-partlabel/").join(p));
        let mut esp_device = None;
        for path in esp_devices {
            if path.exists() {
                esp_device = Some(path);
                break;
            }
        }
        let esp_device = esp_device.ok_or_else(|| anyhow::anyhow!("Failed to find ESP device"))?;
        for &mnt in ESP_MOUNTS.iter() {
            let mnt = root.join(mnt);
            if !mnt.exists() {
                continue;
            }
            let status = std::process::Command::new("mount")
                .arg(&esp_device)
                .arg(&mnt)
                .status()?;
            if !status.success() {
                anyhow::bail!("Failed to mount {:?}", esp_device);
            }
            log::debug!("Mounted at {mnt:?}");
            *mountpoint = Some(mnt);
            break;
        }
        Ok(mountpoint.as_deref().unwrap().to_owned())
    }

    fn unmount(&self) -> Result<()> {
        if let Some(mount) = self.mountpoint.borrow_mut().take() {
            let status = Command::new("umount").arg(&mount).status()?;
            if !status.success() {
                anyhow::bail!("Failed to unmount {mount:?}: {status:?}");
            }
            log::trace!("Unmounted");
        }
        Ok(())
    }

    #[context("Updating EFI firmware variables")]
    fn update_firmware(&self, device: &str, espdir: &openat::Dir, vendordir: &str) -> Result<()> {
        if !is_efi_booted()? {
            log::debug!("Not booted via EFI, skipping firmware update");
            return Ok(());
        }
        clear_efi_current()?;
        set_efi_current(device, espdir, vendordir)
    }
}

/// Convert a nul-terminated UTF-16 byte array to a String.
fn string_from_utf16_bytes(slice: &[u8]) -> String {
    // For some reason, systemd appends 3 nul bytes after the string.
    // Drop the last byte if there's an odd number.
    let size = slice.len() / 2;
    let v: Vec<u16> = (0..size)
        .map(|i| u16::from_ne_bytes([slice[2 * i], slice[2 * i + 1]]))
        .collect();
    U16CString::from_vec(v).unwrap().to_string_lossy()
}

/// Read a nul-terminated UTF-16 string from an EFI variable.
fn read_efi_var_utf16_string(name: &str) -> Option<String> {
    let efivars = Path::new("/sys/firmware/efi/efivars");
    if !efivars.exists() {
        log::warn!("No efivars mount at {:?}", efivars);
        return None;
    }
    let path = efivars.join(name);
    if !path.exists() {
        log::trace!("No EFI variable {name}");
        return None;
    }
    match std::fs::read(&path) {
        Ok(buf) => {
            // Skip the first 4 bytes, those are the EFI variable attributes.
            if buf.len() < 4 {
                log::warn!("Read less than 4 bytes from {:?}", path);
                return None;
            }
            Some(string_from_utf16_bytes(&buf[4..]))
        }
        Err(reason) => {
            log::warn!("Failed reading {:?}: {reason}", path);
            None
        }
    }
}

/// Read the LoaderInfo EFI variable if it exists.
fn get_loader_info() -> Option<String> {
    read_efi_var_utf16_string(LOADER_INFO_VAR_STR)
}

/// Read the StubInfo EFI variable if it exists.
fn get_stub_info() -> Option<String> {
    read_efi_var_utf16_string(STUB_INFO_VAR_STR)
}

/// Whether to skip adoption if a systemd bootloader is found.
fn skip_systemd_bootloaders() -> bool {
    if let Some(loader_info) = get_loader_info() {
        if loader_info.starts_with("systemd") {
            log::trace!("Skipping adoption for {:?}", loader_info);
            return true;
        }
    }
    if let Some(stub_info) = get_stub_info() {
        log::trace!("Skipping adoption for {:?}", stub_info);
        return true;
    }
    false
}

impl Component for Efi {
    fn name(&self) -> &'static str {
        "EFI"
    }

    fn query_adopt(&self) -> Result<Option<Adoptable>> {
        let esp = self.open_esp_optional()?;
        if esp.is_none() {
            log::trace!("No ESP detected");
            return Ok(None);
        };
        // This would be extended with support for other operating systems later
        if let Some(coreos_aleph) = crate::coreos::get_aleph_version(Path::new("/"))? {
            let meta = ContentMetadata {
                timestamp: coreos_aleph.ts,
                version: coreos_aleph.aleph.version,
            };
            log::trace!("EFI adoptable: {:?}", &meta);
            return Ok(Some(Adoptable {
                version: meta,
                confident: true,
            }));
        } else {
            log::trace!("No CoreOS aleph detected");
        }
        // Don't adopt if the system is booted with systemd-boot or
        // systemd-stub since those will be managed with bootctl.
        if skip_systemd_bootloaders() {
            return Ok(None);
        }
        let ostree_deploy_dir = Path::new("/ostree/deploy");
        if ostree_deploy_dir.exists() {
            let btime = ostree_deploy_dir.metadata()?.created()?;
            let timestamp = chrono::DateTime::from(btime);
            let meta = ContentMetadata {
                timestamp,
                version: "unknown".to_string(),
            };
            return Ok(Some(Adoptable {
                version: meta,
                confident: true,
            }));
        }
        Ok(None)
    }

    /// Given an adoptable system and an update, perform the update.
    fn adopt_update(
        &self,
        sysroot: &openat::Dir,
        updatemeta: &ContentMetadata,
    ) -> Result<InstalledContent> {
        let meta = if let Some(meta) = self.query_adopt()? {
            meta
        } else {
            anyhow::bail!("Failed to find adoptable system");
        };

        let esp = self.open_esp()?;
        validate_esp(&esp)?;
        let updated = sysroot
            .sub_dir(&component_updatedirname(self))
            .context("opening update dir")?;
        let updatef = filetree::FileTree::new_from_dir(&updated).context("reading update dir")?;
        // For adoption, we should only touch files that we know about.
        let diff = updatef.relative_diff_to(&esp)?;
        log::trace!("applying adoption diff: {}", &diff);
        filetree::apply_diff(&updated, &esp, &diff, None).context("applying filesystem changes")?;
        Ok(InstalledContent {
            meta: updatemeta.clone(),
            filetree: Some(updatef),
            adopted_from: Some(meta.version),
        })
    }

    // TODO: Remove dest_root; it was never actually used
    fn install(
        &self,
        src_root: &openat::Dir,
        dest_root: &str,
        device: &str,
        update_firmware: bool,
    ) -> Result<InstalledContent> {
        let meta = if let Some(meta) = get_component_update(src_root, self)? {
            meta
        } else {
            anyhow::bail!("No update metadata for component {} found", self.name());
        };
        log::debug!("Found metadata {}", meta.version);
        let srcdir_name = component_updatedirname(self);
        let ft = crate::filetree::FileTree::new_from_dir(&src_root.sub_dir(&srcdir_name)?)?;
        let destdir = &self.ensure_mounted_esp(Path::new(dest_root))?;

        let destd = &openat::Dir::open(destdir)
            .with_context(|| format!("opening dest dir {}", destdir.display()))?;
        validate_esp(destd)?;

        // TODO - add some sort of API that allows directly setting the working
        // directory to a file descriptor.
        let r = std::process::Command::new("cp")
            .args(["-rp", "--reflink=auto"])
            .arg(&srcdir_name)
            .arg(destdir)
            .current_dir(format!("/proc/self/fd/{}", src_root.as_raw_fd()))
            .status()?;
        if !r.success() {
            anyhow::bail!("Failed to copy");
        }
        if update_firmware {
            if let Some(vendordir) = self.get_efi_vendor(&src_root)? {
                self.update_firmware(device, destd, &vendordir)?
            }
        }
        Ok(InstalledContent {
            meta,
            filetree: Some(ft),
            adopted_from: None,
        })
    }

    fn run_update(
        &self,
        sysroot: &openat::Dir,
        current: &InstalledContent,
    ) -> Result<InstalledContent> {
        let currentf = current
            .filetree
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No filetree for installed EFI found!"))?;
        let updatemeta = self.query_update(sysroot)?.expect("update available");
        let updated = sysroot
            .sub_dir(&component_updatedirname(self))
            .context("opening update dir")?;
        let updatef = filetree::FileTree::new_from_dir(&updated).context("reading update dir")?;
        let diff = currentf.diff(&updatef)?;
        self.ensure_mounted_esp(Path::new("/"))?;
        let destdir = self.open_esp().context("opening EFI dir")?;
        validate_esp(&destdir)?;
        log::trace!("applying diff: {}", &diff);
        filetree::apply_diff(&updated, &destdir, &diff, None)
            .context("applying filesystem changes")?;
        let adopted_from = None;
        Ok(InstalledContent {
            meta: updatemeta,
            filetree: Some(updatef),
            adopted_from,
        })
    }

    fn generate_update_metadata(&self, sysroot_path: &str) -> Result<ContentMetadata> {
        let ostreebootdir = Path::new(sysroot_path).join(ostreeutil::BOOT_PREFIX);
        let dest_efidir = component_updatedir(sysroot_path, self);

        if ostreebootdir.exists() {
            let cruft = ["loader", "grub2"];
            for p in cruft.iter() {
                let p = ostreebootdir.join(p);
                if p.exists() {
                    std::fs::remove_dir_all(&p)?;
                }
            }

            let efisrc = ostreebootdir.join("efi/EFI");
            if !efisrc.exists() {
                bail!("Failed to find {:?}", &efisrc);
            }

            // Fork off mv() because on overlayfs one can't rename() a lower level
            // directory today, and this will handle the copy fallback.
            Command::new("mv").args([&efisrc, &dest_efidir]).run()?;
        }

        let efidir = openat::Dir::open(&dest_efidir)?;
        let files = crate::util::filenames(&efidir)?.into_iter().map(|mut f| {
            f.insert_str(0, "/boot/efi/EFI/");
            f
        });

        let meta = packagesystem::query_files(sysroot_path, files)?;
        write_update_metadata(sysroot_path, self, &meta)?;
        Ok(meta)
    }

    fn query_update(&self, sysroot: &openat::Dir) -> Result<Option<ContentMetadata>> {
        get_component_update(sysroot, self)
    }

    fn validate(&self, current: &InstalledContent) -> Result<ValidationResult> {
        let currentf = current
            .filetree
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No filetree for installed EFI found!"))?;
        self.ensure_mounted_esp(Path::new("/"))?;
        let efidir = self.open_esp()?;
        let diff = currentf.relative_diff_to(&efidir)?;
        let mut errs = Vec::new();
        for f in diff.changes.iter() {
            errs.push(format!("Changed: {}", f));
        }
        for f in diff.removals.iter() {
            errs.push(format!("Removed: {}", f));
        }
        assert_eq!(diff.additions.len(), 0);
        if !errs.is_empty() {
            Ok(ValidationResult::Errors(errs))
        } else {
            Ok(ValidationResult::Valid)
        }
    }

    fn get_efi_vendor(&self, sysroot: &openat::Dir) -> Result<Option<String>> {
        let updated = sysroot
            .sub_dir(&component_updatedirname(self))
            .context("opening update dir")?;
        let shim_files = find_file_recursive(updated.recover_path()?, SHIM)?;

        // Does not support multiple shim for efi
        if shim_files.len() > 1 {
            anyhow::bail!("Found multiple {SHIM} in the image");
        }
        if let Some(p) = shim_files.first() {
            let p = p
                .parent()
                .unwrap()
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("No file name found"))?;
            Ok(Some(p.to_string_lossy().into_owned()))
        } else {
            anyhow::bail!("Failed to find {SHIM} in the image")
        }
    }
}

impl Drop for Efi {
    fn drop(&mut self) {
        log::debug!("Unmounting");
        let _ = self.unmount();
    }
}

fn validate_esp(dir: &openat::Dir) -> Result<()> {
    let stat = nix::sys::statfs::fstatfs(dir)?;
    let fstype = stat.filesystem_type();
    if fstype != nix::sys::statfs::MSDOS_SUPER_MAGIC {
        bail!("EFI mount is not a msdos filesystem, but is {:?}", fstype);
    };
    Ok(())
}

#[context("Clearing current EFI boot entry")]
pub(crate) fn clear_efi_current() -> Result<()> {
    const BOOTCURRENT: &str = "BootCurrent";
    if !crate::efi::is_efi_booted()? {
        log::debug!("System is not booted via EFI");
        return Ok(());
    }
    let output = Command::new(EFIBOOTMGR).output()?;
    if !output.status.success() {
        anyhow::bail!("Failed to invoke {EFIBOOTMGR}")
    }
    let output = String::from_utf8(output.stdout)?;
    let current = if let Some(current) = output
        .lines()
        .filter_map(|l| l.split_once(':'))
        .filter_map(|(k, v)| (k == BOOTCURRENT).then_some(v.trim()))
        .next()
    {
        current
    } else {
        log::debug!("No EFI {BOOTCURRENT} found");
        return Ok(());
    };
    log::debug!("EFI current: {current}");
    let output = Command::new(EFIBOOTMGR)
        .args(["-b", current, "-B"])
        .output()?;
    let st = output.status;
    if !st.success() {
        std::io::copy(
            &mut std::io::Cursor::new(output.stderr),
            &mut std::io::stderr().lock(),
        )?;
        anyhow::bail!("Failed to invoke {EFIBOOTMGR}: {st:?}");
    }
    anyhow::Ok(())
}

#[context("Adding new EFI boot entry")]
pub(crate) fn set_efi_current(device: &str, espdir: &openat::Dir, vendordir: &str) -> Result<()> {
    let fsinfo = crate::filesystem::inspect_filesystem(espdir, ".")?;
    let source = fsinfo.source;
    let devname = source
        .rsplit_once('/')
        .ok_or_else(|| anyhow::anyhow!("Failed to parse {source}"))?
        .1;
    let partition_path = format!("/sys/class/block/{devname}/partition");
    let partition_number = std::fs::read_to_string(&partition_path)
        .with_context(|| format!("Failed to read {partition_path}"))?;
    let shim = format!("{vendordir}/{SHIM}");
    if espdir.exists(&shim)? {
        anyhow::bail!("Failed to find {SHIM}");
    }
    let loader = format!("\\EFI\\{}\\{SHIM}", vendordir);
    let st = Command::new(EFIBOOTMGR)
        .args([
            "--create",
            "--disk",
            device,
            "--part",
            partition_number.as_str(),
            "--loader",
            loader.as_str(),
            "--label",
            vendordir,
        ])
        .status()?;
    if !st.success() {
        anyhow::bail!("Failed to invoke {EFIBOOTMGR}")
    }
    anyhow::Ok(())
}

#[context("Find target file recursively")]
fn find_file_recursive<P: AsRef<Path>>(dir: P, target_file: &str) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            if let Some(file_name) = entry.file_name().to_str() {
                if file_name == target_file {
                    if let Some(path) = entry.path().to_str() {
                        result.push(path.into());
                    }
                }
            }
        }
    }

    Ok(result)
}
