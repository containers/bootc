/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{Context, Result};
use fn_error_context::context;
use openat_ext::OpenatDirExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::model::*;

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ValidationResult {
    Valid,
    Errors(Vec<String>),
}

/// A component along with a possible update
pub(crate) trait Component {
    /// Returns the name of the component; this will be used for serialization
    /// and should remain stable.
    fn name(&self) -> &'static str;

    /// In an operating system whose initially booted disk image is not
    /// using bootupd, detect whether it looks like the component exists
    /// and "synthesize" content metadata from it.
    fn query_adopt(&self) -> Result<Option<Adoptable>>;

    /// Given an adoptable system and an update, perform the update.
    fn adopt_update(
        &self,
        sysroot: &openat::Dir,
        update: &ContentMetadata,
    ) -> Result<InstalledContent>;

    /// Implementation of `bootupd install` for a given component.  This should
    /// gather data (or run binaries) from the source root, and install them
    /// into the target root.  It is expected that sub-partitions (e.g. the ESP)
    /// are mounted at the expected place.  For operations that require a block device instead
    /// of a filesystem root, the component should query the mount point to
    /// determine the block device.
    /// This will be run during a disk image build process.
    fn install(&self, src_root: &openat::Dir, dest_root: &str) -> Result<InstalledContent>;

    /// Implementation of `bootupd generate-update-metadata` for a given component.
    /// This expects to be run during an "image update build" process.  For CoreOS
    /// this is an `rpm-ostree compose tree` for example.  For a dual-partition
    /// style updater, this would be run as part of a postprocessing step
    /// while the filesystem for the partition is mounted.
    fn generate_update_metadata(&self, sysroot: &str) -> Result<ContentMetadata>;

    /// Used on the client to query for an update cached in the current booted OS.
    fn query_update(&self, sysroot: &openat::Dir) -> Result<Option<ContentMetadata>>;

    /// Used on the client to run an update.
    fn run_update(
        &self,
        sysroot: &openat::Dir,
        current: &InstalledContent,
    ) -> Result<InstalledContent>;

    /// Used on the client to validate an installed version.
    fn validate(&self, current: &InstalledContent) -> Result<ValidationResult>;
}

/// Given a component name, create an implementation.
pub(crate) fn new_from_name(name: &str) -> Result<Box<dyn Component>> {
    let r: Box<dyn Component> = match name {
        "EFI" => Box::new(crate::efi::Efi::default()),
        "BIOS" => Box::new(crate::bios::Bios::default()),
        _ => anyhow::bail!("No component {}", name),
    };
    Ok(r)
}

/// Returns the path to the payload directory for an available update for
/// a component.
pub(crate) fn component_updatedirname(component: &dyn Component) -> PathBuf {
    Path::new(BOOTUPD_UPDATES_DIR).join(component.name())
}

/// Returns the path to the payload directory for an available update for
/// a component.
pub(crate) fn component_updatedir(sysroot: &str, component: &dyn Component) -> PathBuf {
    Path::new(sysroot).join(component_updatedirname(component))
}

/// Returns the name of the JSON file containing a component's available update metadata installed
/// into the booted operating system root.
fn component_update_data_name(component: &dyn Component) -> PathBuf {
    Path::new(&format!("{}.json", component.name())).into()
}

/// Helper method for writing an update file
pub(crate) fn write_update_metadata(
    sysroot: &str,
    component: &dyn Component,
    meta: &ContentMetadata,
) -> Result<()> {
    let sysroot = openat::Dir::open(sysroot)?;
    let dir = sysroot.sub_dir(BOOTUPD_UPDATES_DIR)?;
    let name = component_update_data_name(component);
    dir.write_file_with(&name, 0o644, |w| -> Result<_> {
        Ok(serde_json::to_writer(w, &meta)?)
    })?;
    Ok(())
}

/// Given a component, return metadata on the available update (if any)
#[context("Loading update for component {}", component.name())]
pub(crate) fn get_component_update(
    sysroot: &openat::Dir,
    component: &dyn Component,
) -> Result<Option<ContentMetadata>> {
    let name = component_update_data_name(component);
    let path = Path::new(BOOTUPD_UPDATES_DIR).join(name);
    if let Some(f) = sysroot.open_file_optional(&path)? {
        let mut f = std::io::BufReader::new(f);
        let u = serde_json::from_reader(&mut f)
            .with_context(|| format!("failed to parse {:?}", &path))?;
        Ok(Some(u))
    } else {
        Ok(None)
    }
}
