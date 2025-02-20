//! Compute the difference between two OSTree commits.

/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0 OR MIT
 */

use anyhow::{Context, Result};
use fn_error_context::context;
use gio::prelude::*;
use ostree::gio;
use std::collections::BTreeSet;
use std::fmt;

/// Like `g_file_query_info()`, but return None if the target doesn't exist.
pub(crate) fn query_info_optional(
    f: &gio::File,
    queryattrs: &str,
    queryflags: gio::FileQueryInfoFlags,
) -> Result<Option<gio::FileInfo>> {
    let cancellable = gio::Cancellable::NONE;
    match f.query_info(queryattrs, queryflags, cancellable) {
        Ok(i) => Ok(Some(i)),
        Err(e) => {
            match e.kind::<gio::IOErrorEnum>() { Some(ref e2) => {
                match e2 {
                    gio::IOErrorEnum::NotFound => Ok(None),
                    _ => Err(e.into()),
                }
            } _ => {
                Err(e.into())
            }}
        }
    }
}

/// A set of file paths.
pub type FileSet = BTreeSet<String>;

/// Diff between two ostree commits.
#[derive(Debug, Default)]
pub struct FileTreeDiff {
    /// The prefix passed for diffing, e.g. /usr
    pub subdir: Option<String>,
    /// Files that are new in an existing directory
    pub added_files: FileSet,
    /// New directories
    pub added_dirs: FileSet,
    /// Files removed
    pub removed_files: FileSet,
    /// Directories removed (recursively)
    pub removed_dirs: FileSet,
    /// Files that changed (in any way, metadata or content)
    pub changed_files: FileSet,
    /// Directories that changed mode/permissions
    pub changed_dirs: FileSet,
}

impl fmt::Display for FileTreeDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "files(added:{} removed:{} changed:{}) dirs(added:{} removed:{} changed:{})",
            self.added_files.len(),
            self.removed_files.len(),
            self.changed_files.len(),
            self.added_dirs.len(),
            self.removed_dirs.len(),
            self.changed_dirs.len()
        )
    }
}

fn diff_recurse(
    prefix: &str,
    diff: &mut FileTreeDiff,
    from: &ostree::RepoFile,
    to: &ostree::RepoFile,
) -> Result<()> {
    let cancellable = gio::Cancellable::NONE;
    let queryattrs = "standard::name,standard::type";
    let queryflags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;
    let from_iter = from.enumerate_children(queryattrs, queryflags, cancellable)?;

    // Iterate over the source (from) directory, and compare with the
    // target (to) directory.  This generates removals and changes.
    while let Some(from_info) = from_iter.next_file(cancellable)? {
        let from_child = from_iter.child(&from_info);
        let name = from_info.name();
        let name = name.to_str().expect("UTF-8 ostree name");
        let path = format!("{prefix}{name}");
        let to_child = to.child(name);
        let to_info = query_info_optional(&to_child, queryattrs, queryflags)
            .context("querying optional to")?;
        let is_dir = matches!(from_info.file_type(), gio::FileType::Directory);
        if to_info.is_some() {
            let to_child = to_child.downcast::<ostree::RepoFile>().expect("downcast");
            to_child.ensure_resolved()?;
            let from_child = from_child.downcast::<ostree::RepoFile>().expect("downcast");
            from_child.ensure_resolved()?;

            if is_dir {
                let from_contents_checksum = from_child.tree_get_contents_checksum();
                let to_contents_checksum = to_child.tree_get_contents_checksum();
                if from_contents_checksum != to_contents_checksum {
                    let subpath = format!("{}/", path);
                    diff_recurse(&subpath, diff, &from_child, &to_child)?;
                }
                let from_meta_checksum = from_child.tree_get_metadata_checksum();
                let to_meta_checksum = to_child.tree_get_metadata_checksum();
                if from_meta_checksum != to_meta_checksum {
                    diff.changed_dirs.insert(path);
                }
            } else {
                let from_checksum = from_child.checksum();
                let to_checksum = to_child.checksum();
                if from_checksum != to_checksum {
                    diff.changed_files.insert(path);
                }
            }
        } else if is_dir {
            diff.removed_dirs.insert(path);
        } else {
            diff.removed_files.insert(path);
        }
    }
    // Iterate over the target (to) directory, and find any
    // files/directories which were not present in the source.
    let to_iter = to.enumerate_children(queryattrs, queryflags, cancellable)?;
    while let Some(to_info) = to_iter.next_file(cancellable)? {
        let name = to_info.name();
        let name = name.to_str().expect("UTF-8 ostree name");
        let path = format!("{prefix}{name}");
        let from_child = from.child(name);
        let from_info = query_info_optional(&from_child, queryattrs, queryflags)
            .context("querying optional from")?;
        if from_info.is_some() {
            continue;
        }
        let is_dir = matches!(to_info.file_type(), gio::FileType::Directory);
        if is_dir {
            diff.added_dirs.insert(path);
        } else {
            diff.added_files.insert(path);
        }
    }
    Ok(())
}

/// Given two ostree commits, compute the diff between them.
#[context("Computing ostree diff")]
pub fn diff<P: AsRef<str>>(
    repo: &ostree::Repo,
    from: &str,
    to: &str,
    subdir: Option<P>,
) -> Result<FileTreeDiff> {
    let subdir = subdir.as_ref();
    let subdir = subdir.map(|s| s.as_ref());
    let (fromroot, _) = repo.read_commit(from, gio::Cancellable::NONE)?;
    let (toroot, _) = repo.read_commit(to, gio::Cancellable::NONE)?;
    let (fromroot, toroot) = if let Some(subdir) = subdir {
        (
            fromroot.resolve_relative_path(subdir),
            toroot.resolve_relative_path(subdir),
        )
    } else {
        (fromroot, toroot)
    };
    let fromroot = fromroot.downcast::<ostree::RepoFile>().expect("downcast");
    fromroot.ensure_resolved()?;
    let toroot = toroot.downcast::<ostree::RepoFile>().expect("downcast");
    toroot.ensure_resolved()?;
    let mut diff = FileTreeDiff {
        subdir: subdir.map(|s| s.to_string()),
        ..Default::default()
    };
    diff_recurse("/", &mut diff, &fromroot, &toroot)?;
    Ok(diff)
}
