/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use chrono::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The directory where updates are stored
pub(crate) const BOOTUPD_UPDATES_DIR: &str = "usr/lib/bootupd/updates";

#[derive(Serialize, Deserialize, Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct ContentMetadata {
    /// The timestamp, which is used to determine update availability
    pub(crate) timestamp: DateTime<Utc>,
    /// Human readable version number, like ostree it is not ever parsed, just displayed
    pub(crate) version: Option<String>,
}

impl ContentMetadata {
    pub(crate) fn compare(&self, other: &Self) -> bool {
        match (self.version.as_ref(), other.version.as_ref()) {
            (Some(a), Some(b)) => a.as_str() == b.as_str(),
            _ => self.timestamp == other.timestamp,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct InstalledContent {
    /// Associated metadata
    pub(crate) meta: ContentMetadata,
    /// Human readable version number, like ostree it is not ever parsed, just displayed
    pub(crate) filetree: Option<crate::filetree::FileTree>,
}

/// Will be serialized into /boot/bootupd-state.json
#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct SavedState {
    /// Maps a component name to its currently installed version
    pub(crate) installed: BTreeMap<String, InstalledContent>,
    /// Maps a component name to an in progress update
    pub(crate) pending: Option<BTreeMap<String, ContentMetadata>>,
}

/// The status of an individual component.
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct ComponentStatus {
    /// Currently installed version
    pub(crate) installed: ContentMetadata,
    /// In progress update that was interrupted
    pub(crate) interrupted: Option<ContentMetadata>,
    /// Update in the deployed filesystem tree
    pub(crate) update: Option<ContentMetadata>,
    /// Is true if the version in `update` is different from `installed`
    pub(crate) updatable: bool,
}

/// Representation of bootupd's worldview at a point in time.
/// This is intended to be a stable format that is output by `bootupctl status --json`
/// and parsed by higher level management tools.  Transitively then
/// everything referenced from here should also be stable.
#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct Status {
    /// Maps a component name to status
    pub(crate) components: BTreeMap<String, ComponentStatus>,
}
