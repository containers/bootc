/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use chrono::prelude::*;
use serde_derive::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::filetree::*;
use crate::sha512string::SHA512String;

/// Metadata for a single file
#[derive(Serialize, Deserialize, Clone, Debug, Hash, Ord, PartialOrd, PartialEq, Eq)]
pub(crate) enum ComponentType {
    #[cfg(any(target_arch = "x86_64", target_arch = "arm"))]
    EFI,
    #[cfg(target_arch = "x86_64")]
    BIOS,
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

impl InstalledContent {
    pub(crate) fn from_file_tree(ft: FileTree) -> InstalledContent {
        InstalledContent {
            digest: ft.digest(),
            timestamp: ft.timestamp,
            filesystem: Some(Box::new(ft)),
        }
    }
}

impl ComponentInstalled {
    pub(crate) fn get_disk_content(&self) -> &InstalledContent {
        match self {
            Self::Unknown(i) => i,
            Self::Tracked { disk, .. } => disk,
        }
    }
}
