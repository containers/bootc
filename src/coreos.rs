//! Bits specific to Fedora CoreOS (and derivatives).

/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{Context, Result};
use chrono::prelude::*;
use openat_ext::OpenatDirExt;
use serde::{Deserialize, Serialize};
use std::fs::{canonicalize, symlink_metadata};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Ord, PartialOrd, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
/// See https://github.com/coreos/fedora-coreos-tracker/blob/66d7d00bedd9d5eabc7287b9577f443dcefb7c04/internals/README-internals.md#aleph-version
pub(crate) struct Aleph {
    #[serde(alias = "build")]
    pub(crate) version: String,
}

pub(crate) struct AlephWithTimestamp {
    pub(crate) aleph: Aleph,
    #[allow(dead_code)]
    pub(crate) ts: chrono::DateTime<Utc>,
}

/// Path to the file, see above
const ALEPH_PATH: &str = "/sysroot/.coreos-aleph-version.json";

pub(crate) fn get_aleph_version() -> Result<Option<AlephWithTimestamp>> {
    let sysroot = openat::Dir::open("/")?;
    let mut path = ALEPH_PATH;
    if !Path::new(ALEPH_PATH).exists() {
        return Ok(None);
    }
    let target;
    let is_link = symlink_metadata(path)
        .with_context(|| format!("reading metadata for {}", path))?
        .file_type()
        .is_symlink();
    if is_link {
        target =
            canonicalize(path).with_context(|| format!("getting absolute path to {}", path))?;
        path = target.to_str().unwrap()
    }
    if let Some(statusf) = sysroot.open_file_optional(path)? {
        let meta = statusf.metadata()?;
        let bufr = std::io::BufReader::new(statusf);
        let aleph: Aleph = serde_json::from_reader(bufr)?;
        Ok(Some(AlephWithTimestamp {
            aleph,
            ts: meta.created()?.into(),
        }))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Result;

    #[test]
    fn test_parse_old_aleph() -> Result<()> {
        // What the aleph file looked like before we changed it in
        // https://github.com/osbuild/osbuild/pull/1475
        let alephdata = r##"
{
    "build": "32.20201002.dev.2",
    "ref": "fedora/x86_64/coreos/testing-devel",
    "ostree-commit": "b2ea6159d6274e1bbbb49aa0ef093eda5d53a75c8a793dbe184f760ed64dc862",
    "imgid": "fedora-coreos-32.20201002.dev.2-qemu.x86_64.qcow2"
}"##;
        let aleph: Aleph = serde_json::from_str(alephdata)?;
        assert_eq!(aleph.version, "32.20201002.dev.2");
        Ok(())
    }

    #[test]
    fn test_parse_aleph() -> Result<()> {
        let alephdata = r##"
{
    "version": "32.20201002.dev.2",
    "ref": "fedora/x86_64/coreos/testing-devel",
    "ostree-commit": "b2ea6159d6274e1bbbb49aa0ef093eda5d53a75c8a793dbe184f760ed64dc862"
}"##;
        let aleph: Aleph = serde_json::from_str(alephdata)?;
        assert_eq!(aleph.version, "32.20201002.dev.2");
        Ok(())
    }
}
