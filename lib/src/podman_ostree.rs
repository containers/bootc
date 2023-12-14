//! # Mapping between podman/containers-storage: and ostree
//!
//! The common container storage model is to store blobs (layers) as unpacked directories,
//! and use the Linux `overlayfs` to merge them dynamically.
//!
//! However, today the `ostree-prepare-root` model as used by ostree expects a final flattened
//! filesystem tree; and crucially we need to perform SELinux labeling.  At the moment, because
//! ostree again works on just a plain directory, we need to "physically" change the on-disk
//! xattrs of the target files.
//!
//! That said, there is work in ostree to use composefs, which will add a huge amount of flexibility;
//! we can generate an erofs blob dynamically with the target labels.
//!
//! Even more than that however the ostree core currently expects an ostree commit object to be backing
//! the filesystem tree; this is how it handles garbage collection, inspects metadata, etc.  Parts
//! of bootc rely on this too today.
//!
//! ## Disadvantages
//!
//! One notable disadvantage of this model is that we're storing file *references* twice,
//! which means the ostree deduplication is pointless.  In theory this is fixable by going back
//! and changing the containers-storage files, but...
//!
//! ## Medium term: Unify containers-storage and ostree with composefs
//!
//! Ultimately the best fix is https://github.com/containers/composefs/issues/125

use std::cell::OnceCell;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};

use cap_std::fs::Dir;
use cap_std::fs::{DirBuilder, DirEntry};
use cap_std::io_lifetimes::AsFilelike;
use cap_std_ext::cap_tempfile::{TempDir, TempFile};
use cap_std_ext::cmdext::CapStdExtCommandExt;
use cap_std_ext::dirext::CapStdExtDirExt;
use cap_std_ext::{cap_primitives, cap_std};
use fn_error_context::context;
use ostree_ext::sysroot::SysrootLock;
use rustix::fd::AsFd;
use std::os::unix::fs::MetadataExt;

use crate::deploy::ImageState;
use crate::podman::PodmanInspectGraphDriver;
use crate::utils::sync_cmd_in_root;

const OSTREE_CONTAINER_IMAGE_REF_PREFIX: &str = "ostree-container/image";

fn image_commit_ostree_ref(imageid: &str) -> String {
    format!("{OSTREE_CONTAINER_IMAGE_REF_PREFIX}/{imageid}")
}

struct MergeState<'a> {
    trash: &'a Dir,
    // Unique integer for naming trashed files
    trashid: AtomicI64,
    can_clone: bool,
}

/// Given one directory entry, perform an overlayfs-style merge operation.
fn merge_one_entry(
    layer: &Dir,
    elt: DirEntry,
    pathbuf: &mut std::path::PathBuf,
    output: &Dir,
    state: &MergeState,
) -> Result<()> {
    let name = elt.file_name();
    // We operate on a shared path buffer for improved efficiency.
    // Here, we append the name of the target file.
    pathbuf.push(&name);
    let src_meta = elt.metadata()?;
    let inum = src_meta.ino();
    let src_ftype = src_meta.file_type();

    // Helper closure which lazily initializes a "layer trash directory" and moves the target path into it.
    let move_to_trash = |src: &Path| -> anyhow::Result<()> {
        let id = state.trashid.fetch_add(1, Ordering::SeqCst);
        let tempname = format!("t{:X}-{:X}", id, inum);
        output
            .rename(src, state.trash, &tempname)
            .with_context(|| format!("Moving {src:?} to trash"))?;
        Ok(())
    };

    let target_meta = output
        .symlink_metadata_optional(&pathbuf)
        .context("Querying target")?;
    if src_ftype.is_dir() {
        // The source layer type is a directory.  Check if we need to create it.
        let mut needs_create = true;
        if let Some(target_meta) = target_meta {
            if target_meta.is_dir() {
                needs_create = false;
            } else {
                // The target exists and is not a directory.  Trash it.
                move_to_trash(&pathbuf)?;
            }
        }
        // Create the directory if needed.
        if needs_create {
            let mut db = DirBuilder::new();
            db.mode(src_meta.mode());
            output
                .create_dir_with(&pathbuf, &db)
                .with_context(|| format!("Creating {pathbuf:?}"))?;
        }
        // Now recurse
        merge_layer(layer, pathbuf, output, state)?;
    } else if (src_meta.mode() & libc::S_IFMT) == libc::S_IFCHR && src_meta.rdev() == 0 {
        // The layer specifies a whiteout entry; remove the target path.
        if target_meta.is_some() {
            move_to_trash(&pathbuf)?;
        }
    } else {
        // We're operating on a non-directory.  In this case if the target exists,
        // it needs to be removed.
        if target_meta.is_some() {
            move_to_trash(&pathbuf)?;
        }
        if src_meta.is_symlink() {
            let target =
                cap_primitives::fs::read_link_contents(&layer.as_filelike_view(), &pathbuf)
                    .with_context(|| format!("Reading link {pathbuf:?}"))?;
            cap_primitives::fs::symlink_contents(target, &output.as_filelike_view(), &pathbuf)
                .with_context(|| format!("Writing symlink {pathbuf:?}"))?;
        } else {
            let src = layer
                .open(&pathbuf)
                .with_context(|| format!("Opening src {pathbuf:?}"))?;
            // Use reflinks if available, otherwise we can fall back to hard linking.  The hardlink
            // count will "leak" into any containers spawned (until podman learns to use composefs).
            if state.can_clone {
                let mut openopts = cap_std::fs::OpenOptions::new();
                openopts.write(true);
                openopts.create_new(true);
                openopts.mode(src_meta.mode());
                let dest = output
                    .open_with(&pathbuf, &openopts)
                    .with_context(|| format!("Opening dest {pathbuf:?}"))?;
                rustix::fs::ioctl_ficlone(dest.as_fd(), src.as_fd()).context("Cloning")?;
            } else {
                layer
                    .hard_link(&pathbuf, output, &pathbuf)
                    .context("Hard linking")?;
            }
        }
    }
    assert!(pathbuf.pop());
    Ok(())
}

/// This function is an "eager" implementation of computing the filesystem tree, implementing
/// the same algorithm as overlayfs, including processing whiteouts.
fn merge_layer(
    layer: &Dir,
    pathbuf: &mut std::path::PathBuf,
    output: &Dir,
    state: &MergeState,
) -> Result<()> {
    for elt in layer.read_dir(&pathbuf)? {
        let elt = elt?;
        merge_one_entry(layer, elt, pathbuf, output, state)?;
    }
    Ok(())
}

#[context("Squashing to tempdir")]
async fn generate_squashed_dir(
    rootfs: &Dir,
    graph: PodmanInspectGraphDriver,
) -> Result<cap_std_ext::cap_tempfile::TempDir> {
    let ostree_tmp = &rootfs.open_dir("ostree/repo/tmp")?;
    let td = TempDir::new_in(ostree_tmp)?;
    // We put files/directories which should be deleted here; they're processed asynchronously
    let trashdir = TempDir::new_in(ostree_tmp)?;
    anyhow::ensure!(graph.name == "overlay");
    let rootfs = rootfs.try_clone()?;
    let td = tokio::task::spawn_blocking(move || {
        let can_clone = OnceCell::<bool>::new();
        for layer in graph.data.layers() {
            // TODO: Does this actually work when operating on a non-default root?
            let layer = layer.trim_start_matches('/');
            tracing::debug!("Merging layer: {layer}");
            let layer = rootfs
                .open_dir(layer)
                .with_context(|| format!("Opening {layer}"))?;
            // Determine if we can do reflinks
            if can_clone.get().is_none() {
                let src = TempFile::new(&layer)?;
                let dest = TempFile::new(&td)?;
                let did_clone =
                    rustix::fs::ioctl_ficlone(dest.as_file().as_fd(), src.as_file().as_fd())
                        .is_ok();
                can_clone.get_or_init(|| did_clone);
            }
            let mut pathbuf = PathBuf::from(".");
            let mergestate = MergeState {
                trash: &trashdir,
                trashid: Default::default(),
                can_clone: *can_clone.get().unwrap(),
            };
            merge_layer(&layer, &mut pathbuf, &td, &mergestate)?;
        }
        anyhow::Ok(td)
    })
    .await??;
    Ok(td)
}

/// Post-process target directory
pub(crate) fn prepare_squashed_root(rootfs: &Dir) -> Result<()> {
    if rootfs.exists("etc") {
        rootfs
            .rename("etc", rootfs, "usr/etc")
            .context("Renaming etc => usr/etc")?;
    }
    // And move everything in /var to the "factory" directory so it can be processed
    // by tmpfiles.d
    if let Some(ref var) = rootfs.open_dir_optional("var")? {
        let factory_var_path = "usr/share/factory/var";
        rootfs.create_dir_all(factory_var_path)?;
        let factory_var = &rootfs.open_dir(factory_var_path)?;
        for ent in var.entries()? {
            let ent = ent?;
            let name = ent.file_name();
            var.rename(&name, factory_var, &name)
                .with_context(|| format!("Moving var/{name:?} to {factory_var_path}"))?;
        }
    }
    Ok(())
}

/// Given an image in containers-storage, generate an ostree commit from it
pub(crate) async fn commit_image_to_ostree(
    sysroot: &SysrootLock,
    imageid: &str,
) -> Result<ImageState> {
    let rootfs = &Dir::reopen_dir(&crate::utils::sysroot_fd_borrowed(sysroot))?;

    // Mount the merged filesystem (via overlayfs) basically just so we can get the final
    // SELinux policy in /etc/selinux which we need to compute the labels
    let cid = crate::podman::temporary_container_for_image(rootfs, imageid).await?;
    let mount_path = &crate::podman::podman_mount(rootfs, &cid).await?;
    // Gather metadata on the image, including its constitutent layers
    let mut inspect = crate::podman::podman_inspect(rootfs, imageid).await?;
    let manifest_digest = inspect.digest;

    // Merge the layers into one final filesystem tree
    let squashed = generate_squashed_dir(rootfs, inspect.graph_driver).await?;
    // Post-process the merged tree
    let squashed = tokio::task::spawn_blocking(move || {
        prepare_squashed_root(&squashed)?;
        anyhow::Ok(squashed)
    })
    .await??;

    tracing::debug!("Writing ostree commit");
    let repo_fd = Arc::new(sysroot.repo().dfd_borrow().try_clone_to_owned()?);
    let ostree_ref = image_commit_ostree_ref(imageid);
    let mut cmd = sync_cmd_in_root(&squashed, "ostree")?;
    cmd.args([
        "--repo=/proc/self/fd/3",
        "commit",
        "--consume",
        "--selinux-policy",
        mount_path.as_str(),
        "--branch",
        ostree_ref.as_str(),
        "--tree=dir=.",
    ]);
    cmd.take_fd_n(repo_fd, 3);
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.kill_on_drop(true);
    let st = cmd.status().await?;
    if !st.success() {
        anyhow::bail!("Failed to ostree commit: {st:?}")
    }
    let ostree_commit = sysroot.repo().require_rev(&ostree_ref)?.to_string();
    Ok(ImageState {
        backend: crate::spec::Backend::Container,
        created: inspect.created,
        manifest_digest,
        version: inspect.config.labels.remove("version"),
        ostree_commit,
    })
}
