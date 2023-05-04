/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use std::cell::RefCell;
use std::io::prelude::*;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use openat_ext::OpenatDirExt;

use crate::component::*;
use crate::filetree;
use crate::model::*;
use crate::ostreeutil;
use crate::util;
use crate::util::CommandRunExt;

/// Well-known paths to the ESP that may have been mounted external to us.
pub(crate) const ESP_MOUNTS: &[&str] = &["boot/efi", "efi"];

/// The ESP partition label on Fedora CoreOS derivatives
pub(crate) const COREOS_ESP_PART_LABEL: &str = "EFI-SYSTEM";
pub(crate) const ANACONDA_ESP_PART_LABEL: &str = "EFI\\x20System\\x20Partition";

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

    fn ensure_mounted_esp(&self, root: &Path) -> Result<PathBuf> {
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
        let tmppath = tempfile::tempdir_in("/tmp")?.into_path();
        let status = std::process::Command::new("mount")
            .arg(&esp_device)
            .arg(&tmppath)
            .status()?;
        if !status.success() {
            anyhow::bail!("Failed to mount {:?}", esp_device);
        }
        log::debug!("Mounted at {tmppath:?}");
        *mountpoint = Some(tmppath);
        Ok(mountpoint.as_deref().unwrap().to_owned())
    }

    fn unmount(&self) -> Result<()> {
        if let Some(mount) = self.mountpoint.borrow_mut().take() {
            let status = Command::new("umount").arg(&mount).status()?;
            if !status.success() {
                anyhow::bail!("Failed to unmount {mount:?}: {status:?}");
            }
        }
        Ok(())
    }
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
        if let Some(coreos_aleph) = crate::coreos::get_aleph_version()? {
            let meta = ContentMetadata {
                timestamp: coreos_aleph.ts,
                version: coreos_aleph.aleph.imgid,
            };
            log::trace!("EFI adoptable: {:?}", &meta);
            return Ok(Some(Adoptable {
                version: meta,
                confident: true,
            }));
        } else {
            log::trace!("No CoreOS aleph detected");
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
        _: &str,
    ) -> Result<InstalledContent> {
        let meta = if let Some(meta) = get_component_update(src_root, self)? {
            meta
        } else {
            anyhow::bail!("No update metadata for component {} found", self.name());
        };
        let srcdir_name = component_updatedirname(self);
        let ft = crate::filetree::FileTree::new_from_dir(&src_root.sub_dir(&srcdir_name)?)?;
        let destdir = &self.ensure_mounted_esp(Path::new(dest_root))?;
        {
            let destd = openat::Dir::open(destdir)
                .with_context(|| format!("opening dest dir {}", destdir.display()))?;
            validate_esp(&destd)?;
        }
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

        // Query the rpm database and list the package and build times for all the
        // files in the EFI system partition. If any files are not owned it is considered
        // and error condition.
        let mut rpmout = util::rpm_query(sysroot_path, &dest_efidir)?;
        let rpmout = rpmout.output()?;
        if !rpmout.status.success() {
            std::io::stderr().write_all(&rpmout.stderr)?;
            bail!("Failed to invoke rpm -qf");
        }

        let meta = util::parse_rpm_metadata(rpmout.stdout)?;
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
}

impl Drop for Efi {
    fn drop(&mut self) {
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
