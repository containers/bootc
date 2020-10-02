/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

use crate::model::*;

#[serde(rename_all = "kebab-case")]
#[derive(Serialize, Deserialize, Debug)]
pub(crate) enum ValidationResult {
    Valid,
    Errors(Vec<String>),
}

/// A component along with a possible update
pub(crate) trait Component {
    /// Returns the name of the component; this will be used for serialization
    /// and should remain stable.
    fn name(&self) -> &'static str;

    /// Implementation of `bootupd install` for a given component.  This should
    /// gather data (or run binaries) from the source root, and install them
    /// into the target root.  It is expected that sub-partitions (e.g. the ESP)
    /// are mounted at the expected place.  For operations that require a block device instead
    /// of a filesystem root, the component should query the mount point to
    /// determine the block device.
    /// This will be run during a disk image build process.
    fn install(&self, src_root: &str, dest_root: &str) -> Result<InstalledContent>;

    /// Implementation of `bootupd generate-update-metadata` for a given component.
    /// This expects to be run during an "image update build" process.  For CoreOS
    /// this is an `rpm-ostree compose tree` for example.  For a dual-partition
    /// style updater, this would be run as part of a postprocessing step
    /// while the filesystem for the partition is mounted.
    fn generate_update_metadata(&self, sysroot: &str) -> Result<ContentMetadata>;

    /// Used on the client to query for an update cached in the current booted OS.
    fn query_update(&self) -> Result<Option<ContentMetadata>>;

    /// Used on the client to run an update.
    fn run_update(&self, current: &InstalledContent) -> Result<InstalledContent>;

    /// Used on the client to validate an installed version.
    fn validate(&self, current: &InstalledContent) -> Result<ValidationResult>;
}

/// Given a component name, create an implementation.
pub(crate) fn new_from_name(name: &str) -> Result<Box<dyn Component>> {
    let r: Box<dyn Component> = match name {
        "EFI" => Box::new(crate::efi::EFI::default()),
        _ => anyhow::bail!("No component {}", name),
    };
    Ok(r)
}

/// Returns the path to the JSON file containing a component's available update metadata installed
/// into the booted operating system root.
pub(crate) fn component_update_metapath(sysroot: &str, component: &dyn Component) -> PathBuf {
    Path::new(sysroot)
        .join(BOOTUPD_UPDATES_DIR)
        .join(format!("{}.json", component.name()))
}

/// Returns the path to the payload directory for an available update for
/// a component.
pub(crate) fn component_updatedir(sysroot: &str, component: &dyn Component) -> PathBuf {
    Path::new(sysroot)
        .join(BOOTUPD_UPDATES_DIR)
        .join(component.name())
}

/// Helper method for writing an update file
pub(crate) fn write_update_metadata(
    sysroot: &str,
    component: &dyn Component,
    meta: &ContentMetadata,
) -> Result<()> {
    let metap = component_update_metapath(sysroot, component);
    let mut f = std::io::BufWriter::new(std::fs::File::create(&metap)?);
    serde_json::to_writer(&mut f, &meta)?;
    f.flush()?;
    Ok(())
}

/// Given a component, return metadata on the available update (if any)
pub(crate) fn get_component_update(
    sysroot: &str,
    component: &dyn Component,
) -> Result<Option<ContentMetadata>> {
    let metap = component_update_metapath(sysroot, component);
    if !metap.exists() {
        return Ok(None);
    }
    let mut f = std::io::BufReader::new(File::open(&metap)?);
    let u = serde_json::from_reader(&mut f)?;
    Ok(Some(u))
}
