/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{bail, Context, Result};
use openat_ext::OpenatDirExt;
use openssl::hash::{Hasher, MessageDigest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Display;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;

/// The prefix we apply to our temporary files.
pub(crate) const TMP_PREFIX: &str = ".btmp.";
// This module doesn't handle modes right now, because
// we're only targeting FAT filesystems for UEFI.
// In FAT there are no unix permission bits, usually
// they're set by mount options.
// See also https://github.com/coreos/fedora-coreos-config/commit/8863c2b34095a2ae5eae6fbbd121768a5f592091
const DEFAULT_FILE_MODE: u32 = 0o700;

use crate::sha512string::SHA512String;

/// Metadata for a single file
#[derive(Clone, Serialize, Deserialize, Debug, Hash, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct FileMetadata {
    /// File size in bytes
    pub(crate) size: u64,
    /// Content checksum; chose SHA-512 because there are not a lot of files here
    /// and it's ok if the checksum is large.
    pub(crate) sha512: SHA512String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct FileTree {
    pub(crate) children: BTreeMap<String, FileMetadata>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FileTreeDiff {
    pub(crate) additions: HashSet<String>,
    pub(crate) removals: HashSet<String>,
    pub(crate) changes: HashSet<String>,
}

impl Display for FileTreeDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(
            f,
            "additions: {} removals: {} changes: {}",
            self.additions.len(),
            self.removals.len(),
            self.changes.len()
        )
    }
}

#[cfg(test)]
impl FileTreeDiff {
    pub(crate) fn count(&self) -> usize {
        self.additions.len() + self.removals.len() + self.changes.len()
    }
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
        let _ = std::io::copy(&mut r, &mut hasher)?;
        let digest = SHA512String::from_hasher(&mut hasher);
        Ok(FileMetadata {
            size: meta.len(),
            sha512: digest,
        })
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
                    let _ = ret.insert(name.to_string(), meta);
                }
                openat::SimpleType::Dir => {
                    let child = dir.sub_dir(name)?;
                    for (mut k, v) in FileTree::unsorted_from_dir(&child)?.drain() {
                        k.reserve(name.len() + 1);
                        k.insert(0, '/');
                        k.insert_str(0, name);
                        let _ = ret.insert(k, v);
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

    /// Create a FileTree from the target directory.
    pub(crate) fn new_from_dir(dir: &openat::Dir) -> Result<Self> {
        let mut children = BTreeMap::new();
        for (k, v) in Self::unsorted_from_dir(dir)?.drain() {
            children.insert(k, v);
        }

        Ok(Self { children })
    }

    /// Determine the changes *from* self to the updated tree
    pub(crate) fn diff(&self, updated: &Self) -> Result<FileTreeDiff> {
        self.diff_impl(updated, true)
    }

    /// Determine any changes only using the files tracked in self as
    /// a reference.  In other words, this will ignore any unknown
    /// files and not count them as additions.
    #[cfg(test)]
    pub(crate) fn changes(&self, current: &Self) -> Result<FileTreeDiff> {
        self.diff_impl(current, false)
    }

    /// The inverse of `changes` - determine if there are any files
    /// changed or added in `current` compared to self.
    #[cfg(test)]
    pub(crate) fn updates(&self, current: &Self) -> Result<FileTreeDiff> {
        current.diff_impl(self, false)
    }

    fn diff_impl(&self, updated: &Self, check_additions: bool) -> Result<FileTreeDiff> {
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
        if check_additions {
            for k in updated.children.keys() {
                if self.children.get(k).is_some() {
                    continue;
                }
                additions.insert(k.clone());
            }
        }
        Ok(FileTreeDiff {
            additions,
            removals,
            changes,
        })
    }

    /// Create a diff from a target directory.  This will ignore
    /// any files or directories that are not part of the original tree.
    pub(crate) fn relative_diff_to(&self, dir: &openat::Dir) -> Result<FileTreeDiff> {
        let mut removals = HashSet::new();
        let mut changes = HashSet::new();

        for (path, info) in self.children.iter() {
            assert!(!path.starts_with('/'));

            if let Some(meta) = dir.metadata_optional(path)? {
                match meta.simple_type() {
                    openat::SimpleType::File => {
                        let target_info = FileMetadata::new_from_path(dir, path)?;
                        if info != &target_info {
                            changes.insert(path.clone());
                        }
                    }
                    _ => {
                        // If a file became a directory
                        changes.insert(path.clone());
                    }
                }
            } else {
                removals.insert(path.clone());
            }
        }
        Ok(FileTreeDiff {
            additions: HashSet::new(),
            removals,
            changes,
        })
    }
}

// Recursively remove all files in the directory that start with our TMP_PREFIX
fn cleanup_tmp(dir: &openat::Dir) -> Result<()> {
    for entry in dir.list_dir(".")? {
        let entry = entry?;
        let name = if let Some(name) = entry.file_name().to_str() {
            name
        } else {
            // Skip invalid UTF-8 for now, we will barf on it later though.
            continue;
        };

        match dir.get_file_type(&entry)? {
            openat::SimpleType::Dir => {
                let child = dir.sub_dir(name)?;
                cleanup_tmp(&child)?;
            }
            openat::SimpleType::File => {
                if name.starts_with(TMP_PREFIX) {
                    dir.remove_file(name)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Default, Clone)]
pub(crate) struct ApplyUpdateOptions {
    pub(crate) skip_removals: bool,
    pub(crate) skip_sync: bool,
}

// syncfs() is a Linux-specific system call, which doesn't seem
// to be bound in nix today.  I found https://github.com/XuShaohua/nc
// but that's a nontrivial dependency with not a lot of code review.
// Let's just fork off a helper process for now.
pub(crate) fn syncfs(d: &openat::Dir) -> Result<()> {
    let d = d.sub_dir(".").expect("subdir");
    let mut c = std::process::Command::new("sync");
    let c = c.args(&["-f", "."]);
    unsafe {
        c.pre_exec(move || {
            nix::unistd::fchdir(d.as_raw_fd()).expect("fchdir");
            Ok(())
        })
    };
    let r = c.status().context("syncfs failed")?;
    if !r.success() {
        bail!("syncfs failed");
    }
    Ok(())
}

fn tmpname_for_path<P: AsRef<Path>>(path: P) -> std::path::PathBuf {
    let path = path.as_ref();
    let mut buf = path.file_name().expect("filename").to_os_string();
    buf.push(TMP_PREFIX);
    path.with_file_name(buf)
}

/// Given two directories, apply a diff generated from srcdir to destdir
pub(crate) fn apply_diff(
    srcdir: &openat::Dir,
    destdir: &openat::Dir,
    diff: &FileTreeDiff,
    opts: Option<&ApplyUpdateOptions>,
) -> Result<()> {
    let default_opts = ApplyUpdateOptions {
        ..Default::default()
    };
    let opts = opts.unwrap_or(&default_opts);
    cleanup_tmp(destdir).context("cleaning up temporary files")?;

    // Write new and changed files
    for pathstr in diff.additions.iter().chain(diff.changes.iter()) {
        let path = Path::new(pathstr);
        if let Some(parent) = path.parent() {
            destdir.ensure_dir_all(parent, DEFAULT_FILE_MODE)?;
        }
        let destp = tmpname_for_path(path);
        srcdir
            .copy_file_at(path, destdir, destp.as_path())
            .with_context(|| format!("writing {}", &pathstr))?;
    }
    // Ensure all of the new files are written persistently to disk
    if !opts.skip_sync {
        syncfs(destdir)?;
    }
    // Now move them all into place (TODO track interruption)
    for path in diff.additions.iter().chain(diff.changes.iter()) {
        let pathtmp = tmpname_for_path(path);
        destdir
            .local_rename(&pathtmp, path)
            .with_context(|| format!("renaming {path}"))?;
    }
    if !opts.skip_removals {
        for path in diff.removals.iter() {
            destdir
                .remove_file(path)
                .with_context(|| format!("removing {path}"))?;
        }
    }
    // A second full filesystem sync to narrow any races rather than
    // waiting for writeback to kick in.
    if !opts.skip_sync {
        syncfs(destdir)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn run_diff(a: &openat::Dir, b: &openat::Dir) -> Result<FileTreeDiff> {
        let ta = FileTree::new_from_dir(&a)?;
        let tb = FileTree::new_from_dir(&b)?;
        let diff = ta.diff(&tb)?;
        Ok(diff)
    }

    fn test_one_apply<AP: AsRef<Path>, BP: AsRef<Path>>(
        a: AP,
        b: BP,
        opts: Option<&ApplyUpdateOptions>,
    ) -> Result<()> {
        let a = a.as_ref();
        let b = b.as_ref();
        let t = tempfile::tempdir()?;
        let c = t.path().join("c");
        let r = std::process::Command::new("cp")
            .arg("-rp")
            .args(&[a, &c])
            .status()?;
        if !r.success() {
            bail!("failed to cp");
        };
        let c = openat::Dir::open(&c)?;
        let da = openat::Dir::open(a)?;
        let db = openat::Dir::open(b)?;
        let ta = FileTree::new_from_dir(&da)?;
        let tb = FileTree::new_from_dir(&db)?;
        let diff = ta.diff(&tb)?;
        let rdiff = tb.diff(&ta)?;
        assert_eq!(diff.count(), rdiff.count());
        assert_eq!(diff.additions.len(), rdiff.removals.len());
        assert_eq!(diff.changes.len(), rdiff.changes.len());
        apply_diff(&db, &c, &diff, opts)?;
        let tc = FileTree::new_from_dir(&c)?;
        let newdiff = tb.diff(&tc)?;
        let skip_removals = opts.map(|o| o.skip_removals).unwrap_or(false);
        if skip_removals {
            let n = newdiff.count();
            if n != 0 {
                assert_eq!(n, diff.removals.len());
            }
            for f in diff.removals.iter() {
                assert!(c.exists(f)?);
                assert!(da.exists(f)?);
            }
        } else {
            assert_eq!(newdiff.count(), 0);
        }
        Ok(())
    }

    fn test_apply<AP: AsRef<Path>, BP: AsRef<Path>>(a: AP, b: BP) -> Result<()> {
        let a = a.as_ref();
        let b = b.as_ref();
        let skip_removals = ApplyUpdateOptions {
            skip_removals: true,
            ..Default::default()
        };
        test_one_apply(&a, &b, None).context("testing apply (with removals)")?;
        test_one_apply(&a, &b, Some(&skip_removals))
            .context("testing apply (skipping removals)")?;
        Ok(())
    }

    #[test]
    fn test_filetree() -> Result<()> {
        let tmpd = tempfile::tempdir()?;
        let p = tmpd.path();
        let pa = p.join("a");
        let pb = p.join("b");
        std::fs::create_dir(&pa)?;
        std::fs::create_dir(&pb)?;
        let a = openat::Dir::open(&pa)?;
        let b = openat::Dir::open(&pb)?;
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.count(), 0);
        a.create_dir("foo", 0o755)?;
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.count(), 0);
        {
            let mut bar = a.write_file("foo/bar", 0o644)?;
            bar.write("foobarcontents".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.count(), 1);
        assert_eq!(diff.removals.len(), 1);
        let ta = FileTree::new_from_dir(&a)?;
        let tb = FileTree::new_from_dir(&b)?;
        let cdiff = ta.changes(&tb)?;
        assert_eq!(cdiff.count(), 1);
        assert_eq!(cdiff.removals.len(), 1);
        let udiff = ta.updates(&tb)?;
        assert_eq!(udiff.count(), 0);
        test_apply(&pa, &pb).context("testing apply 1")?;
        let rdiff = ta.relative_diff_to(&b)?;
        assert_eq!(rdiff.removals.len(), cdiff.removals.len());

        b.create_dir("foo", 0o755)?;
        {
            let mut bar = b.write_file("foo/bar", 0o644)?;
            bar.write("foobarcontents".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.count(), 0);
        test_apply(&pa, &pb).context("testing apply 2")?;
        {
            let mut bar2 = b.write_file("foo/bar", 0o644)?;
            bar2.write("foobarcontents2".as_bytes())?;
        }
        let diff = run_diff(&a, &b)?;
        assert_eq!(diff.count(), 1);
        assert_eq!(diff.changes.len(), 1);
        let ta = FileTree::new_from_dir(&a)?;
        let rdiff = ta.relative_diff_to(&b)?;
        assert_eq!(rdiff.count(), diff.count());
        assert_eq!(rdiff.changes.len(), diff.changes.len());
        test_apply(&pa, &pb).context("testing apply 3")?;
        Ok(())
    }

    #[test]
    fn test_filetree2() -> Result<()> {
        let tmpd = tempfile::tempdir()?;
        let tmpdp = tmpd.path();
        let relp = "EFI/fedora";
        let a = tmpdp.join("a");
        let b = tmpdp.join("b");
        for d in &[&a, &b] {
            let efidir = d.join(relp);
            fs::create_dir_all(&efidir)?;
            let shimdata = "shim data";
            fs::write(efidir.join("shim.x64"), shimdata)?;
            let grubdata = "grub data";
            fs::write(efidir.join("grub.x64"), grubdata)?;
        }
        fs::write(b.join(relp).join("grub.x64"), "grub data 2")?;
        let newsubp = Path::new(relp).join("subdir");
        fs::create_dir_all(b.join(&newsubp))?;
        fs::write(b.join(&newsubp).join("newgrub.x64"), "newgrub data")?;
        fs::remove_file(b.join(&relp).join("shim.x64"))?;
        {
            let a = openat::Dir::open(&a)?;
            let b = openat::Dir::open(&b)?;
            let ta = FileTree::new_from_dir(&a)?;
            let tb = FileTree::new_from_dir(&b)?;
            let diff = ta.diff(&tb)?;
            assert_eq!(diff.changes.len(), 1);
            assert_eq!(diff.additions.len(), 1);
            assert_eq!(diff.count(), 3);
            super::apply_diff(&b, &a, &diff, None)?;
        }
        assert_eq!(
            String::from_utf8(std::fs::read(a.join(relp).join("grub.x64"))?)?,
            "grub data 2"
        );
        assert_eq!(
            String::from_utf8(std::fs::read(a.join(&newsubp).join("newgrub.x64"))?)?,
            "newgrub data"
        );
        assert!(!a.join(&relp).join("shim.x64").exists());
        Ok(())
    }
}
