use anyhow::{anyhow, Result};
use camino::{Utf8Component, Utf8Path};
use fn_error_context::context;
use ostree_ext::{gio, glib, ostree};

/// The default access mode for directories: rwxr-xr-x
const DEFAULT_DIRECTORY_MODE: u32 = 0o755;

/// Generate directory metadata variant for root/root 0755 directory with an optional SELinux label.
#[context("Creating dirmeta")]
pub(crate) fn create_dirmeta(
    path: &Utf8Path,
    sepolicy: Option<&ostree::SePolicy>,
) -> Result<glib::Variant> {
    let finfo = gio::FileInfo::new();
    finfo.set_attribute_uint32("unix::uid", 0);
    finfo.set_attribute_uint32("unix::gid", 0);
    finfo.set_attribute_uint32("unix::mode", libc::S_IFDIR | DEFAULT_DIRECTORY_MODE);
    let xattrs = sepolicy
        .map(|policy| crate::lsm::new_xattrs_with_selinux(policy, path, 0o644))
        .transpose()?;
    Ok(ostree::create_directory_metadata(&finfo, xattrs.as_ref()))
}

/// Wraps [`create_dirmeta`] and commits it, returning the digest.
#[context("Committing dirmeta")]
pub(crate) fn create_and_commit_dirmeta(
    repo: &ostree::Repo,
    path: &Utf8Path,
    sepolicy: Option<&ostree::SePolicy>,
) -> Result<String> {
    let v = create_dirmeta(path, sepolicy)?;
    let r = repo.write_metadata(
        ostree::ObjectType::DirMeta,
        None,
        &v,
        gio::Cancellable::NONE,
    )?;
    Ok(r.to_hex())
}

// Drop any leading / or . from the path,
fn relative_path_components(p: &Utf8Path) -> impl Iterator<Item = Utf8Component> {
    p.components()
        .filter(|p| matches!(p, Utf8Component::Normal(_)))
}

#[context("Creating parents")]
fn ensure_parent_dirs(
    mt: &ostree::MutableTree,
    path: &Utf8Path,
    metadata_checksum: &str,
) -> Result<ostree::MutableTree> {
    let parts = relative_path_components(path)
        .map(|s| s.as_str())
        .collect::<Vec<_>>();
    mt.ensure_parent_dirs(&parts, metadata_checksum)
        .map_err(Into::into)
}

#[context("Writing file to ostree repo")]
pub fn write_file(
    repo: &ostree::Repo,
    root: &ostree::MutableTree,
    path: &Utf8Path,
    parent_dirmeta: &str,
    contents: &[u8],
    mode: u32,
    sepolicy: Option<&ostree::SePolicy>,
) -> Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("Expecting a filename in {path}"))?;
    let parent = if path.parent().is_some() {
        Some(ensure_parent_dirs(root, &path, parent_dirmeta)?)
    } else {
        None
    };
    let parent = parent.as_ref().unwrap_or(root);
    let xattrs = sepolicy
        .map(|policy| crate::lsm::new_xattrs_with_selinux(policy, path, 0o644))
        .transpose()?;
    let xattrs = xattrs.as_ref();
    let checksum = repo.write_regfile_inline(
        None,
        0,
        0,
        libc::S_IFREG | mode,
        xattrs,
        contents,
        gio::Cancellable::NONE,
    )?;
    parent.replace_file(name, checksum.as_str())?;
    Ok(())
}
