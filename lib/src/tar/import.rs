//! APIs for extracting OSTree commits from container images

use crate::variant_utils::variant_new_from_bytes;
use crate::Result;
use anyhow::{anyhow, Context};
use camino::Utf8Path;
use fn_error_context::context;
use futures::prelude::*;
use gio::prelude::*;
use glib::Cast;
use ostree::ContentWriterExt;
use std::collections::HashMap;
use std::convert::TryInto;
use std::io::prelude::*;
use tracing::{event, instrument, Level};

/// Arbitrary limit on xattrs to avoid RAM exhaustion attacks. The actual filesystem limits are often much smaller.
/// See https://en.wikipedia.org/wiki/Extended_file_attributes
/// For example, XFS limits to 614 KiB.
const MAX_XATTR_SIZE: u32 = 1024 * 1024;
/// Limit on metadata objects (dirtree/dirmeta); this is copied
/// from ostree-core.h.  TODO: Bind this in introspection
const MAX_METADATA_SIZE: u32 = 10 * 1024 * 1024;

/// https://stackoverflow.com/questions/258091/when-should-i-use-mmap-for-file-access
const SMALL_REGFILE_SIZE: usize = 127 * 1024;

// Variant formats, see ostree-core.h
// TODO - expose these via introspection
const OSTREE_COMMIT_FORMAT: &str = "(a{sv}aya(say)sstayay)";
const OSTREE_DIRTREE_FORMAT: &str = "(a(say)a(sayay))";
const OSTREE_DIRMETA_FORMAT: &str = "(uuua(ayay))";
const OSTREE_XATTRS_FORMAT: &str = "a(ayay)";

/// State tracker for the importer.  The main goal is to reject multiple
/// commit objects, as well as finding metadata/content before the commit.
#[derive(Debug, PartialEq, Eq)]
enum ImportState {
    Initial,
    Importing(String),
}

/// Statistics from import.
#[derive(Debug, Default)]
struct ImportStats {
    dirtree: u32,
    dirmeta: u32,
    regfile_small: u32,
    regfile_large: u32,
    symlinks: u32,
}

/// Importer machine.
struct Importer<'a> {
    state: ImportState,
    repo: &'a ostree::Repo,
    xattrs: HashMap<String, glib::Variant>,
    next_xattrs: Option<(String, String)>,

    stats: ImportStats,
}

impl<'a> Drop for Importer<'a> {
    fn drop(&mut self) {
        let _ = self.repo.abort_transaction(gio::NONE_CANCELLABLE);
    }
}

/// Validate size/type of a tar header for OSTree metadata object.
fn validate_metadata_header(header: &tar::Header, desc: &str) -> Result<usize> {
    if header.entry_type() != tar::EntryType::Regular {
        return Err(anyhow!("Invalid non-regular metadata object {}", desc));
    }
    let size = header.size()?;
    let max_size = MAX_METADATA_SIZE as u64;
    if size > max_size {
        return Err(anyhow!(
            "object of size {} exceeds {} bytes",
            size,
            max_size
        ));
    }
    Ok(size as usize)
}

fn header_attrs(header: &tar::Header) -> Result<(u32, u32, u32)> {
    let uid: u32 = header.uid()?.try_into()?;
    let gid: u32 = header.gid()?.try_into()?;
    let mode: u32 = header.mode()?.try_into()?;
    Ok((uid, gid, mode))
}

fn format_for_objtype(t: ostree::ObjectType) -> Option<&'static str> {
    match t {
        ostree::ObjectType::DirTree => Some(OSTREE_DIRTREE_FORMAT),
        ostree::ObjectType::DirMeta => Some(OSTREE_DIRMETA_FORMAT),
        ostree::ObjectType::Commit => Some(OSTREE_COMMIT_FORMAT),
        _ => None,
    }
}

/// The C function ostree_object_type_from_string aborts on
/// unknown strings, so we have a safe version here.
fn objtype_from_string(t: &str) -> Option<ostree::ObjectType> {
    Some(match t {
        "commit" => ostree::ObjectType::Commit,
        "dirtree" => ostree::ObjectType::DirTree,
        "dirmeta" => ostree::ObjectType::DirMeta,
        "file" => ostree::ObjectType::File,
        _ => return None,
    })
}

/// Given a tar entry, read it all into a GVariant
fn entry_to_variant<R: std::io::Read>(
    mut entry: tar::Entry<R>,
    vtype: &str,
    desc: &str,
) -> Result<glib::Variant> {
    let header = entry.header();
    let size = validate_metadata_header(header, desc)?;

    let mut buf: Vec<u8> = Vec::with_capacity(size);
    let n = std::io::copy(&mut entry, &mut buf)?;
    assert_eq!(n as usize, size);
    let v = glib::Bytes::from_owned(buf);
    Ok(crate::variant_utils::variant_normal_from_bytes(vtype, v))
}

impl<'a> Importer<'a> {
    /// Import a commit object.  Must be in "initial" state.  This transitions into the "importing" state.
    fn import_commit<R: std::io::Read>(
        &mut self,
        entry: tar::Entry<R>,
        checksum: &str,
    ) -> Result<()> {
        assert_eq!(self.state, ImportState::Initial);
        self.import_metadata(entry, checksum, ostree::ObjectType::Commit)?;
        event!(Level::DEBUG, "Imported {}.commit", checksum);
        self.state = ImportState::Importing(checksum.to_string());
        Ok(())
    }

    /// Import a metadata object.
    fn import_metadata<R: std::io::Read>(
        &mut self,
        entry: tar::Entry<R>,
        checksum: &str,
        objtype: ostree::ObjectType,
    ) -> Result<()> {
        let vtype =
            format_for_objtype(objtype).ok_or_else(|| anyhow!("Unhandled objtype {}", objtype))?;
        let v = entry_to_variant(entry, vtype, checksum)?;
        // FIXME insert expected dirtree/dirmeta
        let _ = self
            .repo
            .write_metadata(objtype, Some(checksum), &v, gio::NONE_CANCELLABLE)?;
        match objtype {
            ostree::ObjectType::DirMeta => self.stats.dirmeta += 1,
            ostree::ObjectType::DirTree => self.stats.dirtree += 1,
            ostree::ObjectType::Commit => {}
            _ => unreachable!(),
        }
        Ok(())
    }

    /// Import a content object.
    fn import_large_regfile_object<R: std::io::Read>(
        &mut self,
        mut entry: tar::Entry<R>,
        size: usize,
        checksum: &str,
        xattrs: Option<glib::Variant>,
    ) -> Result<()> {
        let cancellable = gio::NONE_CANCELLABLE;
        let (uid, gid, mode) = header_attrs(entry.header())?;
        let w = self.repo.write_regfile(
            Some(checksum),
            uid,
            gid,
            libc::S_IFREG | mode,
            size as u64,
            xattrs.as_ref(),
        )?;
        {
            let w = w.clone().upcast::<gio::OutputStream>();
            let mut buf = [0; 8192];
            loop {
                let n = entry.read(&mut buf[..]).context("Reading large regfile")?;
                if n == 0 {
                    break;
                }
                w.write(&buf[0..n], cancellable)
                    .context("Writing large regfile")?;
            }
        }
        let c = w.finish(cancellable)?;
        debug_assert_eq!(c, checksum);
        self.stats.regfile_large += 1;
        Ok(())
    }

    /// Import a content object.
    fn import_small_regfile_object<R: std::io::Read>(
        &mut self,
        mut entry: tar::Entry<R>,
        size: usize,
        checksum: &str,
        xattrs: Option<glib::Variant>,
    ) -> Result<()> {
        let (uid, gid, mode) = header_attrs(entry.header())?;
        assert!(size <= SMALL_REGFILE_SIZE);
        let mut buf = vec![0u8; size];
        entry.read_exact(&mut buf[..])?;
        let c = self.repo.write_regfile_inline(
            Some(checksum),
            uid,
            gid,
            mode,
            xattrs.as_ref(),
            &buf,
            gio::NONE_CANCELLABLE,
        )?;
        debug_assert_eq!(c.as_str(), checksum);
        self.stats.regfile_small += 1;
        Ok(())
    }

    /// Import a content object.
    fn import_symlink_object<R: std::io::Read>(
        &mut self,
        entry: tar::Entry<R>,
        checksum: &str,
        xattrs: Option<glib::Variant>,
    ) -> Result<()> {
        let (uid, gid, _) = header_attrs(entry.header())?;
        let target = entry
            .header()
            .link_name()?
            .ok_or_else(|| anyhow!("Invalid symlink"))?;
        let target = target
            .as_os_str()
            .to_str()
            .ok_or_else(|| anyhow!("Non-utf8 symlink"))?;
        let c = self.repo.write_symlink(
            Some(checksum),
            uid,
            gid,
            xattrs.as_ref(),
            target,
            gio::NONE_CANCELLABLE,
        )?;
        debug_assert_eq!(c.as_str(), checksum);
        self.stats.symlinks += 1;
        Ok(())
    }

    /// Import a content object.
    #[context("Processing content object {}", checksum)]
    fn import_content_object<R: std::io::Read>(
        &mut self,
        entry: tar::Entry<R>,
        checksum: &str,
        xattrs: Option<glib::Variant>,
    ) -> Result<()> {
        let cancellable = gio::NONE_CANCELLABLE;
        if self
            .repo
            .has_object(ostree::ObjectType::File, checksum, cancellable)?
        {
            return Ok(());
        }
        let size: usize = entry.header().size()?.try_into()?;
        match entry.header().entry_type() {
            tar::EntryType::Regular => {
                if size > SMALL_REGFILE_SIZE {
                    self.import_large_regfile_object(entry, size, checksum, xattrs)
                } else {
                    self.import_small_regfile_object(entry, size, checksum, xattrs)
                }
            }
            tar::EntryType::Symlink => self.import_symlink_object(entry, checksum, xattrs),
            o => return Err(anyhow!("Invalid tar entry of type {:?}", o)),
        }
    }

    /// Given a tar entry that looks like an object (its path is under ostree/repo/objects/),
    /// determine its type and import it.
    #[context("object {}", path)]
    fn import_object<'b, R: std::io::Read>(
        &mut self,
        entry: tar::Entry<'b, R>,
        path: &Utf8Path,
    ) -> Result<()> {
        let parentname = path
            .parent()
            .map(|p| p.file_name())
            .flatten()
            .ok_or_else(|| anyhow!("Invalid path (no parent) {}", path))?;
        if parentname.len() != 2 {
            return Err(anyhow!("Invalid checksum parent {}", parentname));
        }
        let mut name = path
            .file_name()
            .map(Utf8Path::new)
            .ok_or_else(|| anyhow!("Invalid path (dir) {}", path))?;
        let mut objtype = name
            .extension()
            .ok_or_else(|| anyhow!("Invalid objpath {}", path))?;
        let is_xattrs = objtype == "xattrs";
        let xattrs = self.next_xattrs.take();
        if is_xattrs {
            if xattrs.is_some() {
                return Err(anyhow!("Found multiple xattrs"));
            }
            name = name
                .file_stem()
                .map(Utf8Path::new)
                .ok_or_else(|| anyhow!("Invalid xattrs {}", path))?;
            objtype = name
                .extension()
                .ok_or_else(|| anyhow!("Invalid objpath {}", path))?;
        }
        let checksum_rest = name
            .file_stem()
            .ok_or_else(|| anyhow!("Invalid objpath {}", path))?;

        if checksum_rest.len() != 62 {
            return Err(anyhow!("Invalid checksum rest {}", name));
        }
        let checksum = format!("{}{}", parentname, checksum_rest);
        validate_sha256(&checksum)?;
        let xattr_ref = if let Some((xattr_target, xattr_objref)) = xattrs {
            if xattr_target.as_str() != checksum.as_str() {
                return Err(anyhow!(
                    "Found object {} but previous xattr was {}",
                    checksum,
                    xattr_target
                ));
            }
            let v = self
                .xattrs
                .get(&xattr_objref)
                .ok_or_else(|| anyhow!("Failed to find xattr {}", xattr_objref))?;
            Some(v.clone())
        } else {
            None
        };
        let objtype = objtype_from_string(&objtype)
            .ok_or_else(|| anyhow!("Invalid object type {}", objtype))?;
        match (objtype, is_xattrs, &self.state) {
            (ostree::ObjectType::Commit, _, ImportState::Initial) => {
                self.import_commit(entry, &checksum)
            }
            (ostree::ObjectType::File, true, ImportState::Importing(_)) => {
                self.import_xattr_ref(entry, checksum)
            }
            (ostree::ObjectType::File, false, ImportState::Importing(_)) => {
                self.import_content_object(entry, &checksum, xattr_ref)
            }
            (objtype, false, ImportState::Importing(_)) => {
                self.import_metadata(entry, &checksum, objtype)
            }
            (o, _, ImportState::Initial) => {
                return Err(anyhow!("Found content object {} before commit", o))
            }
            (ostree::ObjectType::Commit, _, ImportState::Importing(c)) => {
                return Err(anyhow!("Found multiple commit objects; original: {}", c))
            }
            (objtype, true, _) => {
                return Err(anyhow!("Found xattrs for non-file object type {}", objtype))
            }
        }
    }

    /// Handle <checksum>.xattr hardlinks that contain extended attributes for
    /// a content object.
    #[context("Processing xattr ref")]
    fn import_xattr_ref<'b, R: std::io::Read>(
        &mut self,
        entry: tar::Entry<'b, R>,
        target: String,
    ) -> Result<()> {
        assert!(self.next_xattrs.is_none());
        let header = entry.header();
        if header.entry_type() != tar::EntryType::Link {
            return Err(anyhow!("Non-hardlink xattr reference found for {}", target));
        }
        let xattr_target = entry
            .link_name()?
            .ok_or_else(|| anyhow!("No xattr link content for {}", target))?;
        let xattr_target = Utf8Path::from_path(&*xattr_target)
            .ok_or_else(|| anyhow!("Invalid non-UTF8 xattr link {}", target))?;
        let xattr_target = xattr_target
            .file_name()
            .ok_or_else(|| anyhow!("Invalid xattr link {}", target))?;
        validate_sha256(xattr_target)?;
        self.next_xattrs = Some((target, xattr_target.to_string()));
        Ok(())
    }

    /// Process a special /xattrs/ entry (sha256 of xattr values).
    fn import_xattrs<'b, R: std::io::Read>(&mut self, mut entry: tar::Entry<'b, R>) -> Result<()> {
        match &self.state {
            ImportState::Initial => return Err(anyhow!("Found xattr object {} before commit")),
            ImportState::Importing(_) => {}
        }
        let checksum = {
            let path = entry.path()?;
            let name = path
                .file_name()
                .ok_or_else(|| anyhow!("Invalid xattr dir: {:?}", path))?;
            let name = name
                .to_str()
                .ok_or_else(|| anyhow!("Invalid non-UTF8 xattr name: {:?}", name))?;
            validate_sha256(name)?;
            name.to_string()
        };
        let header = entry.header();
        if header.entry_type() != tar::EntryType::Regular {
            return Err(anyhow!(
                "Invalid xattr entry of type {:?}",
                header.entry_type()
            ));
        }
        let n = header.size()?;
        if n > MAX_XATTR_SIZE as u64 {
            return Err(anyhow!("Invalid xattr size {}", n));
        }

        let mut contents = vec![0u8; n as usize];
        entry.read_exact(contents.as_mut_slice())?;
        let contents: glib::Bytes = contents.as_slice().into();
        let contents = variant_new_from_bytes(OSTREE_XATTRS_FORMAT, contents, false);

        self.xattrs.insert(checksum, contents);
        Ok(())
    }

    /// Consume this importer and return the imported OSTree commit checksum.
    fn commit(mut self) -> Result<String> {
        self.repo.commit_transaction(gio::NONE_CANCELLABLE)?;
        match std::mem::replace(&mut self.state, ImportState::Initial) {
            ImportState::Importing(c) => Ok(c),
            ImportState::Initial => Err(anyhow!("Failed to find a commit object to import")),
        }
    }
}

fn validate_sha256(s: &str) -> Result<()> {
    if s.len() != 64 {
        return Err(anyhow!("Invalid sha256 checksum (len) {}", s));
    }
    if !s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        return Err(anyhow!("Invalid sha256 checksum {}", s));
    }
    Ok(())
}

/// Read the contents of a tarball and import the ostree commit inside.  The sha56 of the imported commit will be returned.
#[instrument(skip(repo, src))]
pub async fn import_tar(
    repo: &ostree::Repo,
    src: impl tokio::io::AsyncRead + Send + Unpin + 'static,
) -> Result<String> {
    let (pipein, copydriver) = crate::async_util::copy_async_read_to_sync_pipe(src)?;
    let repo = repo.clone();
    let import = tokio::task::spawn_blocking(move || {
        let repo = &repo;
        let mut importer = Importer {
            state: ImportState::Initial,
            repo,
            xattrs: Default::default(),
            next_xattrs: None,
            stats: Default::default(),
        };
        repo.prepare_transaction(gio::NONE_CANCELLABLE)?;
        let mut archive = tar::Archive::new(pipein);
        for entry in archive.entries()? {
            let entry = entry?;
            if entry.header().entry_type() == tar::EntryType::Directory {
                continue;
            }
            let path = entry.path()?;
            let path = &*path;
            let path = Utf8Path::from_path(path)
                .ok_or_else(|| anyhow!("Invalid non-utf8 path {:?}", path))?;
            let path = if let Ok(p) = path.strip_prefix("sysroot/ostree/repo/") {
                p
            } else {
                continue;
            };

            if let Ok(p) = path.strip_prefix("objects/") {
                // Need to clone here, otherwise we borrow from the moved entry
                let p = &p.to_owned();
                importer.import_object(entry, p)?;
            } else if let Ok(_) = path.strip_prefix("xattrs/") {
                importer.import_xattrs(entry)?;
            }
        }

        importer.commit()
    })
    .map_err(anyhow::Error::msg);
    let (import, _copydriver) = tokio::try_join!(import, copydriver)?;
    let import = import?;
    Ok(import)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_sha256() -> Result<()> {
        validate_sha256("a86d80a3e9ff77c2e3144c787b7769b300f91ffd770221aac27bab854960b964")?;
        assert!(validate_sha256("").is_err());
        assert!(validate_sha256(
            "a86d80a3e9ff77c2e3144c787b7769b300f91ffd770221aac27bab854960b9644"
        )
        .is_err());
        assert!(validate_sha256(
            "a86d80a3E9ff77c2e3144c787b7769b300f91ffd770221aac27bab854960b964"
        )
        .is_err());
        Ok(())
    }
}
