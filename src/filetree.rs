/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{bail, Result};
use byteorder::ByteOrder;
use chrono::prelude::*;
use openat_ext::OpenatDirExt;
use openssl::hash::{Hasher, MessageDigest};
use serde_derive::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
// Seems like a rust-analyzer bug?
#[allow(unused_imports)]
use std::io::Write;

/// The prefix we apply to our temporary files.
pub(crate) const TMP_PREFIX: &'static str = ".btmp.";

use crate::sha512string::SHA512String;

/// Metadata for a single file
#[derive(Serialize, Debug, Hash, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct FileMetadata {
    /// File size in bytes
    pub(crate) size: u64,
    /// Content checksum; chose SHA-512 because there are not a lot of files here
    /// and it's ok if the checksum is large.
    pub(crate) sha512: SHA512String,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct FileTree {
    pub(crate) timestamp: NaiveDateTime,
    pub(crate) children: BTreeMap<String, FileMetadata>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FileTreeDiff {
    pub(crate) additions: HashSet<String>,
    pub(crate) removals: HashSet<String>,
    pub(crate) changes: HashSet<String>,
}

impl FileMetadata {
    pub(crate) fn new_from_path<P: openat::AsPath>(
        dir: &openat::Dir,
        name: P,
    ) -> Result<FileMetadata> {
        let mut r = dir.open_file(name)?;
        let meta = r.metadata()?;
        let mut hasher =
            Hasher::new(MessageDigest::sha512()).expect("openssl sha512 hasher creation failed");
        std::io::copy(&mut r, &mut hasher)?;
        let digest = SHA512String::from_hasher(&mut hasher);
        Ok(FileMetadata {
            size: meta.len(),
            sha512: digest,
        })
    }

    pub(crate) fn extend_hash(&self, hasher: &mut Hasher) {
        let mut lenbuf = [0; 8];
        byteorder::BigEndian::write_u64(&mut lenbuf, self.size);
        hasher.update(&lenbuf).unwrap();
        hasher.update(&self.sha512.digest_bytes()).unwrap();
    }
}

impl FileTree {
    // Internal helper to generate a sub-tree
    fn unsorted_from_dir(dir: &openat::Dir) -> Result<HashMap<String, FileMetadata>> {
        let mut ret = HashMap::new();
        for entry in dir.list_dir(".")? {
            let entry = entry?;
            let name = if let Some(name) = entry.file_name().to_str() {
                name
            } else {
                bail!("Invalid UTF-8 filename: {:?}", entry.file_name())
            };
            if name.starts_with(TMP_PREFIX) {
                bail!("File {} contains our temporary prefix!", name);
            }
            match dir.get_file_type(&entry)? {
                openat::SimpleType::File => {
                    let meta = FileMetadata::new_from_path(dir, name)?;
                    ret.insert(name.to_string(), meta);
                }
                openat::SimpleType::Dir => {
                    let child = dir.sub_dir(name)?;
                    for (mut k, v) in FileTree::unsorted_from_dir(&child)?.drain() {
                        k.reserve(name.len() + 1);
                        k.insert(0, '/');
                        k.insert_str(0, name);
                        ret.insert(k, v);
                    }
                }
                openat::SimpleType::Symlink => {
                    bail!("Unsupported symbolic link {:?}", entry.file_name())
                }
                openat::SimpleType::Other => {
                    bail!("Unsupported non-file/directory {:?}", entry.file_name())
                }
            }
        }
        Ok(ret)
    }

    pub(crate) fn digest(&self) -> SHA512String {
        let mut hasher =
            Hasher::new(MessageDigest::sha512()).expect("openssl sha512 hasher creation failed");
        for (k, v) in self.children.iter() {
            hasher.update(k.as_bytes()).unwrap();
            v.extend_hash(&mut hasher);
        }
        SHA512String::from_hasher(&mut hasher)
    }

    /// Create a FileTree from the target directory.
    pub(crate) fn new_from_dir(dir: &openat::Dir) -> Result<Self> {
        let mut children = BTreeMap::new();
        for (k, v) in Self::unsorted_from_dir(dir)?.drain() {
            children.insert(k, v);
        }

        let meta = dir.metadata(".")?;
        let stat = meta.stat();

        Ok(Self {
            timestamp: chrono::NaiveDateTime::from_timestamp(stat.st_mtime, 0),
            children: children,
        })
    }

    /// Determine the changes *from* self to the updated tree
    pub(crate) fn diff(&self, updated: &Self) -> Result<FileTreeDiff> {
        let mut additions = HashSet::new();
        let mut removals = HashSet::new();
        let mut changes = HashSet::new();

        for (k, v1) in self.children.iter() {
            if let Some(v2) = updated.children.get(k) {
                if v1 != v2 {
                    changes.insert(k.clone());
                }
            } else {
                removals.insert(k.clone());
            }
        }
        for k in updated.children.keys() {
            if let Some(_) = self.children.get(k) {
                continue;
            }
            additions.insert(k.clone());
        }
        Ok(FileTreeDiff {
            additions: additions,
            removals: removals,
            changes: changes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_diff(a: &openat::Dir, b: &openat::Dir) -> Result<FileTreeDiff> {
        let ta = FileTree::new_from_dir(&a)?;
        let tb = FileTree::new_from_dir(&b)?;
        let diff = ta.diff(&tb)?;
        Ok(diff)
    }

    #[test]
    fn test_diff() -> Result<()> {
        let tmpd = tempfile::tempdir()?;
        let p = tmpd.path();
        let a = p.join("a");
        let b = p.join("b");
        std::fs::create_dir(&a)?;
        std::fs::create_dir(&b)?;
        let a = openat::Dir::open(&a)?;
        let b = openat::Dir::open(&b)?;
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 0);
        assert_eq!(diff.changes.len(), 0);
        a.create_dir("foo", 0o755)?;
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 0);
        assert_eq!(diff.changes.len(), 0);
        {
            let mut bar = a.write_file("foo/bar", 0o644)?;
            bar.write("foobarcontents".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 1);
        assert_eq!(diff.changes.len(), 0);
        b.create_dir("foo", 0o755)?;
        {
            let mut bar = b.write_file("foo/bar", 0o644)?;
            bar.write("foobarcontents".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 0);
        assert_eq!(diff.changes.len(), 0);
        {
            let mut bar2 = b.write_file("foo/bar", 0o644)?;
            bar2.write("foobarcontents2".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.additions.len(), 0);
        assert_eq!(diff.removals.len(), 0);
        assert_eq!(diff.changes.len(), 1);
        Ok(())
    }
}
