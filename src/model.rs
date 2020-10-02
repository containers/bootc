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
#[serde(rename_all = "kebab-case")]
pub(crate) struct ContentMetadata {
    /// The timestamp, which is used to determine update availability
    pub(crate) timestamp: DateTime<Utc>,
    /// Human readable version number, like ostree it is not ever parsed, just displayed
    pub(crate) version: String,
}

impl ContentMetadata {
    /// Returns `true` if `target` is different and chronologically newer
    pub(crate) fn can_upgrade_to(&self, target: &Self) -> bool {
        if self.version == target.version {
            return false;
        }
        return target.timestamp > self.timestamp;
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "kebab-case")]
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
pub(crate) enum ComponentUpdatable {
    NoUpdateAvailable,
    AtLatestVersion,
    Upgradable,
    WouldDowngrade,
}

impl ComponentUpdatable {
    pub(crate) fn from_metadata(from: &ContentMetadata, to: Option<&ContentMetadata>) -> Self {
        match to {
            Some(to) => {
                if from.version == to.version {
                    ComponentUpdatable::AtLatestVersion
                } else if from.can_upgrade_to(to) {
                    ComponentUpdatable::Upgradable
                } else {
                    ComponentUpdatable::WouldDowngrade
                }
            }
            None => ComponentUpdatable::NoUpdateAvailable,
        }
    }
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
    pub(crate) updatable: ComponentUpdatable,
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

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Result;
    use chrono::Duration;

    #[test]
    fn test_meta_compare() {
        let t = Utc::now();
        let a = ContentMetadata {
            timestamp: t,
            version: "v1".into(),
        };
        let b = ContentMetadata {
            timestamp: t + Duration::seconds(1),
            version: "v2".into(),
        };
        assert!(a.can_upgrade_to(&b));
        assert!(!b.can_upgrade_to(&a));
    }

    /// Validate we're not breaking the serialized format.
    #[test]
    fn test_deserialize() -> Result<()> {
        let data = include_str!("../tests/fixtures/example-state-v0.json");
        let state: SavedState = serde_json::from_str(data)?;
        let efi = state.installed.get("EFI").expect("EFI");
        assert_eq!(
            efi.meta.version,
            "grub2-efi-x64-1:2.04-23.fc32.x86_64,shim-x64-15-8.x86_64"
        );
        Ok(())
    }
}
