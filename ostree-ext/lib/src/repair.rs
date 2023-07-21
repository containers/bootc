//! System repair functionality

use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

use anyhow::{anyhow, Context, Result};
use cap_std::fs::Dir;
use cap_std_ext::prelude::CapStdExtCommandExt;
use cap_tempfile::cap_std;
use fn_error_context::context;
use ostree::{gio, glib};
use std::os::unix::fs::MetadataExt;

use crate::sysroot::SysrootLock;

// Find the inode numbers for objects
fn gather_inodes(
    prefix: &str,
    dir: &Dir,
    little_inodes: &mut BTreeMap<u32, String>,
    big_inodes: &mut BTreeMap<u64, String>,
) -> Result<()> {
    for child in dir.entries()? {
        let child = child?;
        let metadata = child.metadata()?;
        if !(metadata.is_file() || metadata.is_symlink()) {
            continue;
        }
        let name = child.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid {name:?}"))?;
        let object_rest = name
            .split_once('.')
            .ok_or_else(|| anyhow!("Invalid object {name}"))?
            .0;
        let checksum = format!("{prefix}{object_rest}");
        let inode = metadata.ino();
        if let Ok(little) = u32::try_from(inode) {
            little_inodes.insert(little, checksum);
        } else {
            big_inodes.insert(inode, checksum);
        }
    }
    Ok(())
}

#[context("Analyzing commit for derivation")]
fn commit_is_derived(commit: &glib::Variant) -> Result<bool> {
    let commit_meta = &glib::VariantDict::new(Some(&commit.child_value(0)));
    if commit_meta
        .lookup::<String>(crate::container::store::META_MANIFEST_DIGEST)?
        .is_some()
    {
        return Ok(true);
    }
    if commit_meta
        .lookup::<bool>("rpmostree.clientlayer")?
        .is_some()
    {
        return Ok(true);
    }
    Ok(false)
}

/// The result of a check_repair operation
#[derive(Debug, PartialEq, Eq)]
pub enum InodeCheckResult {
    /// Problems are unlikely.
    Okay,
    /// There is potential corruption
    PotentialCorruption(BTreeSet<u64>),
}

#[context("Checking inodes")]
#[doc(hidden)]
/// Detect if any commits are potentially incorrect due to inode truncations.
pub fn check_inode_collision(repo: &ostree::Repo, verbose: bool) -> Result<InodeCheckResult> {
    let repo_dir = repo.dfd_as_dir()?;
    let objects = repo_dir.open_dir("objects")?;

    println!(
        r#"Attempting analysis of ostree state for files that may be incorrectly linked.
For more information, see https://github.com/ostreedev/ostree/pull/2874/commits/de6fddc6adee09a93901243dc7074090828a1912
"#
    );

    println!("Gathering inodes for ostree objects...");
    let mut little_inodes = BTreeMap::new();
    let mut big_inodes = BTreeMap::new();

    for child in objects.entries()? {
        let child = child?;
        if !child.file_type()?.is_dir() {
            continue;
        }
        let name = child.file_name();
        if name.len() != 2 {
            continue;
        }
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid {name:?}"))?;
        let objdir = child.open_dir()?;
        gather_inodes(name, &objdir, &mut little_inodes, &mut big_inodes)
            .with_context(|| format!("Processing {name:?}"))?;
    }

    let mut colliding_inodes = BTreeMap::new();
    for (big_inum, big_inum_checksum) in big_inodes.iter() {
        let truncated = *big_inum as u32;
        if let Some(small_inum_object) = little_inodes.get(&truncated) {
            // Don't output each collision unless verbose mode is enabled.  It's actually
            // quite interesting to see data, but only for development and deep introspection
            // use cases.
            if verbose {
                eprintln!(
                    r#"collision:
  inode (>32 bit): {big_inum}
  object: {big_inum_checksum}
  inode (truncated): {truncated}
  object: {small_inum_object}
"#
                );
            }
            colliding_inodes.insert(big_inum, big_inum_checksum);
        }
    }

    let n_big = big_inodes.len();
    let n_small = little_inodes.len();
    println!("Analyzed {n_big} objects with > 32 bit inode numbers and {n_small} objects with <= 32 bit inode numbers");
    if !colliding_inodes.is_empty() {
        return Ok(InodeCheckResult::PotentialCorruption(
            colliding_inodes
                .keys()
                .map(|&&v| v)
                .collect::<BTreeSet<u64>>(),
        ));
    }

    Ok(InodeCheckResult::Okay)
}

/// Attempt to automatically repair any corruption from inode collisions.
#[doc(hidden)]
pub fn auto_repair_inode_collision(
    sysroot: &SysrootLock,
    dry_run: bool,
    verbose: bool,
) -> Result<()> {
    use crate::container::store as container_store;
    let repo = &sysroot.repo();
    let repo_dir = repo.dfd_as_dir()?;

    let mut derived_commits = BTreeSet::new();
    for (_refname, digest) in repo.list_refs(None, gio::Cancellable::NONE)? {
        let commit = repo.load_commit(&digest)?.0;
        if commit_is_derived(&commit)? {
            if verbose {
                eprintln!("Found derived commit: {commit}");
            }
            derived_commits.insert(digest);
        }
    }

    // This is not an ironclad guarantee...however, I am pretty confident that there's
    // no exposure without derivation today.
    if derived_commits.is_empty() {
        println!("OK no derived commits found.");
        return Ok(());
    }
    let n_derived = derived_commits.len();
    println!("Found {n_derived} derived commits");
    println!("Backing filesystem information:");
    {
        let st = Command::new("stat")
            .args(["-f", "."])
            .cwd_dir(repo_dir.try_clone()?)
            .status()?;
        if !st.success() {
            eprintln!("failed to spawn stat: {st:?}");
        }
    }

    match check_inode_collision(repo, verbose)? {
        InodeCheckResult::Okay => {
            println!("OK no colliding inodes found");
            Ok(())
        }
        InodeCheckResult::PotentialCorruption(colliding_inodes) => {
            eprintln!(
                "warning: {} potentially colliding inodes found",
                colliding_inodes.len()
            );
            let all_images = container_store::list_images(repo)?;
            let all_images = all_images
                .into_iter()
                .map(|img| crate::container::ImageReference::try_from(img.as_str()))
                .collect::<Result<Vec<_>>>()?;
            println!("Verifying {} ostree-container images", all_images.len());
            let mut corrupted_images = Vec::new();
            for imgref in all_images {
                if !container_store::verify_container_image(
                    sysroot,
                    &imgref,
                    &colliding_inodes,
                    verbose,
                )? {
                    eprintln!("warning: Corrupted image {imgref}");
                    corrupted_images.push(imgref);
                }
            }
            if corrupted_images.is_empty() {
                println!("OK no corrupted images found");
                return Ok(());
            }
            if dry_run {
                anyhow::bail!("Found potential corruption, dry-run mode enabled");
            }
            container_store::remove_images(repo, corrupted_images.iter())?;
            Ok(())
        }
    }
}
