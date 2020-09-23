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
    fn name(&self) -> &'static str;

    fn install(&self, src_root: &str, dest_root: &str) -> Result<InstalledContent>;

    fn generate_update_metadata(&self, sysroot: &str) -> Result<ContentMetadata>;

    fn query_update(&self) -> Result<Option<ContentMetadata>>;

    fn run_update(&self, current: &InstalledContent) -> Result<InstalledContent>;

    fn validate(&self, current: &InstalledContent) -> Result<ValidationResult>;
}

pub(crate) fn new_from_name(name: &str) -> Result<Box<dyn Component>> {
    let r: Box<dyn Component> = match name {
        "EFI" => Box::new(crate::efi::EFI::default()),
        _ => anyhow::bail!("No component {}", name),
    };
    Ok(r)
}

pub(crate) fn component_update_metapath(sysroot: &str, component: &dyn Component) -> PathBuf {
    Path::new(sysroot)
        .join(BOOTUPD_UPDATES_DIR)
        .join(format!("{}.json", component.name()))
}

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
