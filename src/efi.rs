/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::{BTreeMap, BTreeSet};
use std::io::prelude::*;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use chrono::prelude::*;

use crate::component::*;
use crate::filetree;
use crate::model::*;
use crate::ostreeutil;
use crate::util;
use crate::util::CommandRunExt;

/// The path to the ESP mount
pub(crate) const MOUNT_PATH: &str = "boot/efi";

#[derive(Default)]
pub(crate) struct EFI {}

impl Component for EFI {
    fn name(&self) -> &'static str {
        "EFI"
    }

    fn install(&self, src_root: &str, dest_root: &str) -> Result<InstalledContent> {
        let meta = if let Some(meta) = get_component_update(src_root, self)? {
            meta
        } else {
            anyhow::bail!("No update metadata for component {} found", self.name());
        };
        let srcdir = component_updatedir(src_root, self);
        let srcd = openat::Dir::open(&srcdir)?;
        let ft = crate::filetree::FileTree::new_from_dir(&srcd)?;
        let destdir = Path::new(dest_root).join(MOUNT_PATH);
        {
            let destd = openat::Dir::open(&destdir)?;
            validate_esp(&destd)?;
        }
        let r = std::process::Command::new("cp")
            .args(&["-rp", "--reflink=auto"])
            .arg(&srcdir)
            .arg(&destdir)
            .status()?;
        if !r.success() {
            anyhow::bail!("Failed to copy");
        }
        Ok(InstalledContent {
            meta,
            filetree: Some(ft),
        })
    }

    fn run_update(&self, current: &InstalledContent) -> Result<InstalledContent> {
        let currentf = current
            .filetree
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No filetree for installed EFI found!"))?;
        let updatemeta = self.query_update()?.expect("update available");
        let updated = openat::Dir::open(&component_updatedir("/", self))?;
        let updatef = filetree::FileTree::new_from_dir(&updated)?;
        let diff = currentf.diff(&updatef)?;
        let destdir = openat::Dir::open(&Path::new("/").join(MOUNT_PATH).join("EFI"))?;
        validate_esp(&destdir)?;
        filetree::apply_diff(&updated, &destdir, &diff, None)?;
        Ok(InstalledContent {
            meta: updatemeta,
            filetree: Some(updatef),
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
            let parent = dest_efidir
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Expected parent directory"))?;
            std::fs::create_dir_all(&parent)?;
            Command::new("mv").args(&[&efisrc, &dest_efidir]).run()?;
        }

        let src_efidir = openat::Dir::open(&dest_efidir)?;
        // Query the rpm database and list the package and build times for all the
        // files in the EFI system partition. If any files are not owned it is considered
        // and error condition.
        let rpmout = {
            let mut c = ostreeutil::rpm_cmd(sysroot_path);
            c.args(&["-q", "--queryformat", "%{nevra},%{buildtime} ", "-f"]);
            c.args(util::filenames(&src_efidir)?.drain().map(|mut f| {
                f.insert_str(0, "/boot/efi/EFI/");
                f
            }));
            c
        }
        .output()?;
        if !rpmout.status.success() {
            std::io::stderr().write_all(&rpmout.stderr)?;
            bail!("Failed to invoke rpm -qf");
        }
        let pkgs = std::str::from_utf8(&rpmout.stdout)?
            .split_whitespace()
            .map(|s| -> Result<_> {
                let parts: Vec<_> = s.splitn(2, ',').collect();
                let name = parts[0];
                if let Some(ts) = parts.get(1) {
                    let nt = NaiveDateTime::parse_from_str(ts, "%s")
                        .context("Failed to parse rpm buildtime")?;
                    Ok((name, DateTime::<Utc>::from_utc(nt, Utc)))
                } else {
                    bail!("Failed to parse: {}", s);
                }
            })
            .collect::<Result<BTreeMap<&str, DateTime<Utc>>>>()?;
        if pkgs.is_empty() {
            bail!("Failed to find any RPM packages matching files in source efidir");
        }
        let timestamps: BTreeSet<&DateTime<Utc>> = pkgs.values().collect();
        // Unwrap safety: We validated pkgs has at least one value above
        let largest_timestamp = timestamps.iter().last().unwrap();
        let version = pkgs.keys().fold("".to_string(), |mut s, n| {
            if !s.is_empty() {
                s.push(',');
            }
            s.push_str(n);
            s
        });

        let meta = ContentMetadata {
            timestamp: **largest_timestamp,
            version: version,
        };
        write_update_metadata(sysroot_path, self, &meta)?;
        Ok(meta)
    }

    fn query_update(&self) -> Result<Option<ContentMetadata>> {
        get_component_update("/", self)
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
