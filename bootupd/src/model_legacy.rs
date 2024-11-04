/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

//! Implementation of the original bootupd data format, which is the same
//! as the current one except that the date is defined to be in UTC.

use crate::model::ContentMetadata as NewContentMetadata;
use crate::model::InstalledContent as NewInstalledContent;
use crate::model::SavedState as NewSavedState;
use chrono::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Clone, Debug, Hash, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct ContentMetadata01 {
    /// The timestamp, which is used to determine update availability
    pub(crate) timestamp: NaiveDateTime,
    /// Human readable version number, like ostree it is not ever parsed, just displayed
    pub(crate) version: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct InstalledContent01 {
    /// Associated metadata
    pub(crate) meta: ContentMetadata01,
    /// File tree
    pub(crate) filetree: Option<crate::filetree::FileTree>,
}

/// Will be serialized into /boot/bootupd-state.json
#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedState01 {
    /// Maps a component name to its currently installed version
    pub(crate) installed: BTreeMap<String, InstalledContent01>,
    /// Maps a component name to an in progress update
    pub(crate) pending: Option<BTreeMap<String, ContentMetadata01>>,
}

impl ContentMetadata01 {
    pub(crate) fn upconvert(self) -> NewContentMetadata {
        let timestamp = self.timestamp.and_utc();
        NewContentMetadata {
            timestamp,
            version: self.version,
        }
    }
}

impl InstalledContent01 {
    pub(crate) fn upconvert(self) -> NewInstalledContent {
        NewInstalledContent {
            meta: self.meta.upconvert(),
            filetree: self.filetree,
            adopted_from: None,
        }
    }
}

impl SavedState01 {
    pub(crate) fn upconvert(self) -> NewSavedState {
        let mut r: NewSavedState = Default::default();
        for (k, v) in self.installed {
            r.installed.insert(k, v.upconvert());
        }
        r
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Result;

    /// Validate we're not breaking the serialized format of `bootupctl status --json`
    #[test]
    fn test_deserialize_status() -> Result<()> {
        let data = include_str!("../tests/fixtures/example-state-v0-legacy.json");
        let state: SavedState01 = serde_json::from_str(data)?;
        let efi = state.installed.get("EFI").expect("EFI");
        assert_eq!(
            efi.meta.version,
            "grub2-efi-x64-1:2.04-23.fc32.x86_64,shim-x64-15-8.x86_64"
        );
        let state: NewSavedState = state.upconvert();
        let efi = state.installed.get("EFI").expect("EFI");
        let t = chrono::DateTime::parse_from_rfc3339("2020-09-15T13:01:21Z")?;
        assert_eq!(t, efi.meta.timestamp);
        assert_eq!(
            efi.meta.version,
            "grub2-efi-x64-1:2.04-23.fc32.x86_64,shim-x64-15-8.x86_64"
        );
        Ok(())
    }
}
