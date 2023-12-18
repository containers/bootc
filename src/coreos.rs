//! Bits specific to Fedora CoreOS (and derivatives).

/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{Context, Result};
use chrono::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs::File;
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
const ALEPH_PATH: &str = "sysroot/.coreos-aleph-version.json";

pub(crate) fn get_aleph_version(root: &Path) -> Result<Option<AlephWithTimestamp>> {
    let path = &root.join(ALEPH_PATH);
    if !path.exists() {
        return Ok(None);
    }
    let statusf = File::open(path).with_context(|| format!("Opening {path:?}"))?;
    let meta = statusf.metadata()?;
    let bufr = std::io::BufReader::new(statusf);
    let aleph: Aleph = serde_json::from_reader(bufr)?;
    Ok(Some(AlephWithTimestamp {
        aleph,
        ts: meta.created()?.into(),
    }))
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Result;

    const V1_ALEPH_DATA: &str = r##"
    {
        "version": "32.20201002.dev.2",
        "ref": "fedora/x86_64/coreos/testing-devel",
        "ostree-commit": "b2ea6159d6274e1bbbb49aa0ef093eda5d53a75c8a793dbe184f760ed64dc862"
    }"##;

    #[test]
    fn test_parse_from_root_empty() -> Result<()> {
        // Verify we're a no-op in an empty root
        let root: &tempfile::TempDir = &tempfile::tempdir()?;
        let root = root.path();
        assert!(get_aleph_version(root).unwrap().is_none());
        Ok(())
    }

    #[test]
    fn test_parse_from_root() -> Result<()> {
        let root: &tempfile::TempDir = &tempfile::tempdir()?;
        let root = root.path();
        let sysroot = &root.join("sysroot");
        std::fs::create_dir(sysroot).context("Creating sysroot")?;
        std::fs::write(root.join(ALEPH_PATH), V1_ALEPH_DATA).context("Writing aleph")?;
        let aleph = get_aleph_version(root).unwrap().unwrap();
        assert_eq!(aleph.aleph.version, "32.20201002.dev.2");
        Ok(())
    }

    #[test]
    fn test_parse_from_root_linked() -> Result<()> {
        let root: &tempfile::TempDir = &tempfile::tempdir()?;
        let root = root.path();
        let sysroot = &root.join("sysroot");
        std::fs::create_dir(sysroot).context("Creating sysroot")?;
        let target_name = ".new-ostree-aleph.json";
        let target = &sysroot.join(target_name);
        std::fs::write(root.join(target), V1_ALEPH_DATA).context("Writing aleph")?;
        std::os::unix::fs::symlink(target_name, root.join(ALEPH_PATH)).context("Symlinking")?;
        let aleph = get_aleph_version(root).unwrap().unwrap();
        assert_eq!(aleph.aleph.version, "32.20201002.dev.2");
        Ok(())
    }

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
        let aleph: Aleph = serde_json::from_str(V1_ALEPH_DATA)?;
        assert_eq!(aleph.version, "32.20201002.dev.2");
        Ok(())
    }
}
