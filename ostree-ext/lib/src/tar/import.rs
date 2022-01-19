//! APIs for extracting OSTree commits from container images

use crate::Result;
use anyhow::{anyhow, Context};
use camino::Utf8Path;
use camino::Utf8PathBuf;
use fn_error_context::context;
use gio::glib;
use gio::prelude::*;
use glib::Variant;
use ostree::gio;
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
pub(crate) const SMALL_REGFILE_SIZE: usize = 127 * 1024;

// The prefix for filenames that contain content we actually look at.
const REPO_PREFIX: &str = "sysroot/ostree/repo/";
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
struct Importer {
    repo: ostree::Repo,
    remote: Option<String>,
    xattrs: HashMap<String, glib::Variant>,
    next_xattrs: Option<(String, String)>,

    // Reusable buffer for reads.  See also https://github.com/rust-lang/rust/issues/78485
    buf: Vec<u8>,

    stats: ImportStats,
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
    let mode: u32 = header.mode()?;
    Ok((uid, gid, mode))
}

/// The C function ostree_object_type_from_string aborts on
/// unknown strings, so we have a safe version here.
fn objtype_from_string(t: &str) -> Option<ostree::ObjectType> {
    Some(match t {
        "commit" => ostree::ObjectType::Commit,
        "commitmeta" => ostree::ObjectType::CommitMeta,
        "dirtree" => ostree::ObjectType::DirTree,
        "dirmeta" => ostree::ObjectType::DirMeta,
        "file" => ostree::ObjectType::File,
        _ => return None,
    })
}

/// Given a tar entry, read it all into a GVariant
fn entry_to_variant<R: std::io::Read, T: StaticVariantType>(
    mut entry: tar::Entry<R>,
    desc: &str,
) -> Result<glib::Variant> {
    let header = entry.header();
    let size = validate_metadata_header(header, desc)?;

    let mut buf: Vec<u8> = Vec::with_capacity(size);
    let n = std::io::copy(&mut entry, &mut buf)?;
    assert_eq!(n as usize, size);
    let v = glib::Bytes::from_owned(buf);
    let v = Variant::from_bytes::<T>(&v);
    Ok(v.normal_form())
}

/// Parse an object path into (parent, rest, objtype).
/// Normal ostree object paths look like 00/1234.commit.
/// In the tar format, we may also see 00/1234.file.xattrs.
fn parse_object_entry_path(path: &Utf8Path) -> Result<(&str, &Utf8Path, &str)> {
    // The "sharded" commit directory.
    let parentname = path
        .parent()
        .map(|p| p.file_name())
        .flatten()
        .ok_or_else(|| anyhow!("Invalid path (no parent) {}", path))?;
    if parentname.len() != 2 {
        return Err(anyhow!("Invalid checksum parent {}", parentname));
    }
    let name = path
        .file_name()
        .map(Utf8Path::new)
        .ok_or_else(|| anyhow!("Invalid path (dir) {}", path))?;
    let objtype = name
        .extension()
        .ok_or_else(|| anyhow!("Invalid objpath {}", path))?;
    Ok((parentname, name, objtype))
}

fn parse_checksum(parent: &str, name: &Utf8Path) -> Result<String> {
    let checksum_rest = name
        .file_stem()
        .ok_or_else(|| anyhow!("Invalid object path part {}", name))?;

    if checksum_rest.len() != 62 {
        return Err(anyhow!("Invalid checksum part {}", checksum_rest));
    }
    let checksum = format!("{}{}", parent, checksum_rest);
    validate_sha256(&checksum)?;
    Ok(checksum)
}

impl Importer {
    fn new(repo: &ostree::Repo, remote: Option<String>) -> Self {
        Self {
            repo: repo.clone(),
            remote,
            buf: vec![0u8; 16384],
            xattrs: Default::default(),
            next_xattrs: None,
            stats: Default::default(),
        }
    }

    // Given a tar entry, filter it out if it doesn't look like an object file in
    // `/sysroot/ostree`.
    // It is an error if the filename is invalid UTF-8.  If it is valid UTF-8, return
    // an owned copy of the path.
    fn filter_entry<R: std::io::Read>(
        e: tar::Entry<R>,
    ) -> Result<Option<(tar::Entry<R>, Utf8PathBuf)>> {
        if e.header().entry_type() == tar::EntryType::Directory {
            return Ok(None);
        }
        let orig_path = e.path()?;
        let path = Utf8Path::from_path(&*orig_path)
            .ok_or_else(|| anyhow!("Invalid non-utf8 path {:?}", orig_path))?;
        // Ignore the regular non-object file hardlinks we inject
        if let Ok(path) = path.strip_prefix(REPO_PREFIX) {
            // Filter out the repo config file
            if path.file_name() == Some("config") {
                return Ok(None);
            }
            let path = path.into();
            Ok(Some((e, path)))
        } else {
            Ok(None)
        }
    }

    fn parse_metadata_entry(path: &Utf8Path) -> Result<(String, ostree::ObjectType)> {
        let (parentname, name, objtype) = parse_object_entry_path(path)?;
        let checksum = parse_checksum(parentname, name)?;
        let objtype = objtype_from_string(objtype)
            .ok_or_else(|| anyhow!("Invalid object type {}", objtype))?;
        Ok((checksum, objtype))
    }

    /// Import a metadata object.
    fn import_metadata<R: std::io::Read>(
        &mut self,
        entry: tar::Entry<R>,
        checksum: &str,
        objtype: ostree::ObjectType,
    ) -> Result<()> {
        let v = match objtype {
            ostree::ObjectType::DirTree => {
                self.stats.dirtree += 1;
                entry_to_variant::<_, ostree::TreeVariantType>(entry, checksum)?
            }
            ostree::ObjectType::DirMeta => {
                self.stats.dirmeta += 1;
                entry_to_variant::<_, ostree::DirmetaVariantType>(entry, checksum)?
            }
            o => return Err(anyhow!("Invalid metadata object type; {:?}", o)),
        };
        // FIXME validate here that this checksum was in the set we expected.
        // https://github.com/ostreedev/ostree-rs-ext/issues/1
        let actual =
            self.repo
                .write_metadata(objtype, Some(checksum), &v, gio::NONE_CANCELLABLE)?;
        assert_eq!(actual.to_hex(), checksum);
        Ok(())
    }

    /// Import a content object.
    fn import_large_regfile_object<R: std::io::Read>(
        &mut self,
        mut entry: tar::Entry<R>,
        size: usize,
        checksum: &str,
        xattrs: Option<glib::Variant>,
        cancellable: Option<&gio::Cancellable>,
    ) -> Result<()> {
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
            loop {
                let n = entry
                    .read(&mut self.buf[..])
                    .context("Reading large regfile")?;
                if n == 0 {
                    break;
                }
                w.write(&self.buf[0..n], cancellable)
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
        cancellable: Option<&gio::Cancellable>,
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
            cancellable,
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
        cancellable: Option<&gio::Cancellable>,
    ) -> Result<()> {
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
                    self.import_large_regfile_object(entry, size, checksum, xattrs, cancellable)
                } else {
                    self.import_small_regfile_object(entry, size, checksum, xattrs, cancellable)
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
        cancellable: Option<&gio::Cancellable>,
    ) -> Result<()> {
        let (parentname, mut name, mut objtype) = parse_object_entry_path(path)?;

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
        let checksum = parse_checksum(parentname, name)?;
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
        let objtype = objtype_from_string(objtype)
            .ok_or_else(|| anyhow!("Invalid object type {}", objtype))?;
        if is_xattrs && objtype != ostree::ObjectType::File {
            return Err(anyhow!("Found xattrs for non-file object type {}", objtype));
        }
        match objtype {
            ostree::ObjectType::Commit => Err(anyhow!("Found multiple commit objects")),
            ostree::ObjectType::File => {
                if is_xattrs {
                    self.import_xattr_ref(entry, checksum)
                } else {
                    self.import_content_object(entry, &checksum, xattr_ref, cancellable)
                }
            }
            objtype => self.import_metadata(entry, &checksum, objtype),
        }
    }

    /// Handle <checksum>.xattr hardlinks that contain extended attributes for
    /// a content object.
    #[context("Processing xattr ref")]
    fn import_xattr_ref<R: std::io::Read>(
        &mut self,
        entry: tar::Entry<R>,
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
    fn import_xattrs<R: std::io::Read>(&mut self, mut entry: tar::Entry<R>) -> Result<()> {
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
        let contents = Variant::from_bytes::<&[(&[u8], &[u8])]>(&contents);

        self.xattrs.insert(checksum, contents);
        Ok(())
    }

    fn import(
        mut self,
        archive: &mut tar::Archive<impl Read + Send + Unpin>,
        cancellable: Option<&gio::Cancellable>,
    ) -> Result<String> {
        // Create an iterator that skips over directories; we just care about the file names.
        let mut ents = archive.entries()?.filter_map(|e| match e {
            Ok(e) => Self::filter_entry(e).transpose(),
            Err(e) => Some(Err(anyhow::Error::msg(e))),
        });

        // Read the commit object.
        let (commit_ent, commit_path) = ents
            .next()
            .ok_or_else(|| anyhow!("Commit object not found"))??;

        if commit_ent.header().entry_type() != tar::EntryType::Regular {
            return Err(anyhow!(
                "Expected regular file for commit object, not {:?}",
                commit_ent.header().entry_type()
            ));
        }
        let (checksum, objtype) = Self::parse_metadata_entry(&commit_path)?;
        if objtype != ostree::ObjectType::Commit {
            return Err(anyhow!("Expected commit object, not {:?}", objtype));
        }
        let commit = entry_to_variant::<_, ostree::CommitVariantType>(commit_ent, &checksum)?;

        let (next_ent, nextent_path) = ents
            .next()
            .ok_or_else(|| anyhow!("End of stream after commit object"))??;
        let (next_checksum, next_objtype) = Self::parse_metadata_entry(&nextent_path)?;

        if let Some(remote) = self.remote.as_deref() {
            if next_checksum != checksum {
                return Err(anyhow!(
                    "Expected commitmeta checksum {}, found {}",
                    checksum,
                    next_checksum
                ));
            }
            if next_objtype != ostree::ObjectType::CommitMeta {
                return Err(anyhow!(
                    "Using remote {} for verification; Expected commitmeta object, not {:?}",
                    remote,
                    objtype
                ));
            }
            let commitmeta = entry_to_variant::<_, std::collections::HashMap<String, glib::Variant>>(
                next_ent,
                &next_checksum,
            )?;

            // Now that we have both the commit and detached metadata in memory, verify that
            // the signatures in the detached metadata correctly sign the commit.
            self.repo.signature_verify_commit_data(
                remote,
                &commit.data_as_bytes(),
                &commitmeta.data_as_bytes(),
                ostree::RepoVerifyFlags::empty(),
            )?;

            self.repo.mark_commit_partial(&checksum, true)?;

            // Write the commit object, which also verifies its checksum.
            let actual_checksum =
                self.repo
                    .write_metadata(objtype, Some(&checksum), &commit, cancellable)?;
            assert_eq!(actual_checksum.to_hex(), checksum);
            event!(Level::DEBUG, "Imported {}.commit", checksum);

            // Finally, write the detached metadata.
            self.repo
                .write_commit_detached_metadata(&checksum, Some(&commitmeta), cancellable)?;
        } else {
            self.repo.mark_commit_partial(&checksum, true)?;

            // We're not doing any validation of the commit, so go ahead and write it.
            let actual_checksum =
                self.repo
                    .write_metadata(objtype, Some(&checksum), &commit, cancellable)?;
            assert_eq!(actual_checksum.to_hex(), checksum);
            event!(Level::DEBUG, "Imported {}.commit", checksum);

            // Write the next object, whether it's commit metadata or not.
            let (meta_checksum, meta_objtype) = Self::parse_metadata_entry(&nextent_path)?;
            match meta_objtype {
                ostree::ObjectType::CommitMeta => {
                    let commitmeta = entry_to_variant::<
                        _,
                        std::collections::HashMap<String, glib::Variant>,
                    >(next_ent, &meta_checksum)?;
                    self.repo.write_commit_detached_metadata(
                        &checksum,
                        Some(&commitmeta),
                        gio::NONE_CANCELLABLE,
                    )?;
                }
                _ => {
                    self.import_object(next_ent, &nextent_path, cancellable)?;
                }
            }
        }

        for entry in ents {
            let (entry, path) = entry?;

            if let Ok(p) = path.strip_prefix("objects/") {
                self.import_object(entry, p, cancellable)?;
            } else if path.strip_prefix("xattrs/").is_ok() {
                self.import_xattrs(entry)?;
            }
        }

        Ok(checksum)
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

/// Configuration for tar import.
#[derive(Debug, Default)]
pub struct TarImportOptions {
    /// Name of the remote to use for signature verification.
    pub remote: Option<String>,
}

/// Read the contents of a tarball and import the ostree commit inside.  The sha56 of the imported commit will be returned.
#[instrument(skip(repo, src))]
pub async fn import_tar(
    repo: &ostree::Repo,
    src: impl tokio::io::AsyncRead + Send + Unpin + 'static,
    options: Option<TarImportOptions>,
) -> Result<String> {
    let options = options.unwrap_or_default();
    let src = tokio_util::io::SyncIoBridge::new(src);
    let repo = repo.clone();
    let import = crate::tokio_util::spawn_blocking_cancellable_flatten(move |cancellable| {
        let mut archive = tar::Archive::new(src);
        let txn = repo.auto_transaction(Some(cancellable))?;
        let importer = Importer::new(&repo, options.remote);
        let checksum = importer.import(&mut archive, Some(cancellable))?;
        txn.commit(Some(cancellable))?;
        repo.mark_commit_partial(&checksum, false)?;
        Ok::<_, anyhow::Error>(checksum)
    });
    let import: String = import.await?;
    Ok(import)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_metadata_entry() {
        let c = "a8/6d80a3e9ff77c2e3144c787b7769b300f91ffd770221aac27bab854960b964";
        let invalid = format!("{}.blah", c);
        for &k in &["", "42", c, &invalid] {
            assert!(Importer::parse_metadata_entry(k.into()).is_err())
        }
        let valid = format!("{}.commit", c);
        let r = Importer::parse_metadata_entry(valid.as_str().into()).unwrap();
        assert_eq!(r.0, c.replace('/', ""));
        assert_eq!(r.1, ostree::ObjectType::Commit);
    }

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
