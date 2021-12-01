//! APIs for creating container images from OSTree commits

use crate::objgv::*;
use anyhow::Context;
use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use gio::glib;
use gio::prelude::*;
use gvariant::aligned_bytes::TryAsAligned;
use gvariant::{Marker, Structure};
use ostree::gio;
use std::borrow::Cow;
use std::collections::HashSet;
use std::io::BufReader;

// This way the default ostree -> sysroot/ostree symlink works.
const OSTREEDIR: &str = "sysroot/ostree";

/// A decently large buffer, as used by e.g. coreutils `cat`.
/// System calls are expensive.
const BUF_CAPACITY: usize = 131072;

/// Convert /usr/etc back to /etc
fn map_path(p: &Utf8Path) -> std::borrow::Cow<Utf8Path> {
    match p.strip_prefix("./usr/etc") {
        Ok(r) => Cow::Owned(Utf8Path::new("./etc").join(r)),
        _ => Cow::Borrowed(p),
    }
}

struct OstreeTarWriter<'a, W: std::io::Write> {
    repo: &'a ostree::Repo,
    out: &'a mut tar::Builder<W>,
    wrote_initdirs: bool,
    wrote_dirtree: HashSet<String>,
    wrote_dirmeta: HashSet<String>,
    wrote_content: HashSet<String>,
    wrote_xattrs: HashSet<String>,
}

fn object_path(objtype: ostree::ObjectType, checksum: &str) -> Utf8PathBuf {
    let suffix = match objtype {
        ostree::ObjectType::Commit => "commit",
        ostree::ObjectType::CommitMeta => "commitmeta",
        ostree::ObjectType::DirTree => "dirtree",
        ostree::ObjectType::DirMeta => "dirmeta",
        ostree::ObjectType::File => "file",
        o => panic!("Unexpected object type: {:?}", o),
    };
    let (first, rest) = checksum.split_at(2);
    format!("{}/repo/objects/{}/{}.{}", OSTREEDIR, first, rest, suffix).into()
}

fn xattrs_path(checksum: &str) -> Utf8PathBuf {
    format!("{}/repo/xattrs/{}", OSTREEDIR, checksum).into()
}

impl<'a, W: std::io::Write> OstreeTarWriter<'a, W> {
    fn new(repo: &'a ostree::Repo, out: &'a mut tar::Builder<W>) -> Self {
        Self {
            repo,
            out,
            wrote_initdirs: false,
            wrote_dirmeta: HashSet::new(),
            wrote_dirtree: HashSet::new(),
            wrote_content: HashSet::new(),
            wrote_xattrs: HashSet::new(),
        }
    }

    /// Add a directory entry with default permissions (root/root 0755)
    fn append_default_dir(&mut self, path: &Utf8Path) -> Result<()> {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mode(0o755);
        h.set_size(0);
        self.out.append_data(&mut h, &path, &mut std::io::empty())?;
        Ok(())
    }

    /// Write the initial directory structure.
    fn write_initial_directories(&mut self) -> Result<()> {
        if self.wrote_initdirs {
            return Ok(());
        }
        self.wrote_initdirs = true;

        let objdir: Utf8PathBuf = format!("{}/repo/objects", OSTREEDIR).into();
        // Add all parent directories
        let parent_dirs = {
            let mut parts: Vec<_> = objdir.ancestors().collect();
            parts.reverse();
            parts
        };
        for path in parent_dirs {
            match path.as_str() {
                "/" | "" => continue,
                _ => {}
            }
            self.append_default_dir(path)?;
        }
        // Object subdirectories
        for d in 0..0xFF {
            let path: Utf8PathBuf = format!("{}/{:02x}", objdir, d).into();
            self.append_default_dir(&path)?;
        }

        // The special `repo/xattrs` directory used only in our tar serialization.
        let path: Utf8PathBuf = format!("{}/repo/xattrs", OSTREEDIR).into();
        self.append_default_dir(&path)?;
        Ok(())
    }

    /// Recursively serialize a commit object to the target tar stream.
    fn write_commit(&mut self, checksum: &str) -> Result<()> {
        let cancellable = gio::NONE_CANCELLABLE;

        self.write_initial_directories()?;

        let (commit_v, _) = self.repo.load_commit(checksum)?;
        let commit_v = &commit_v;
        self.append(ostree::ObjectType::Commit, checksum, commit_v)?;

        if let Some(commitmeta) = self
            .repo
            .read_commit_detached_metadata(checksum, cancellable)?
        {
            self.append(ostree::ObjectType::CommitMeta, checksum, &commitmeta)?;
        }

        let commit_v = commit_v.data_as_bytes();
        let commit_v = commit_v.try_as_aligned()?;
        let commit = gv_commit!().cast(commit_v);
        let commit = commit.to_tuple();
        let contents = &hex::encode(commit.6);
        let metadata_checksum = &hex::encode(commit.7);
        let metadata_v = self
            .repo
            .load_variant(ostree::ObjectType::DirMeta, metadata_checksum)?;
        self.append(ostree::ObjectType::DirMeta, metadata_checksum, &metadata_v)?;
        self.append_dirtree(Utf8Path::new("./"), contents, cancellable)?;
        Ok(())
    }

    fn append(
        &mut self,
        objtype: ostree::ObjectType,
        checksum: &str,
        v: &glib::Variant,
    ) -> Result<()> {
        let set = match objtype {
            ostree::ObjectType::Commit | ostree::ObjectType::CommitMeta => None,
            ostree::ObjectType::DirTree => Some(&mut self.wrote_dirtree),
            ostree::ObjectType::DirMeta => Some(&mut self.wrote_dirmeta),
            o => panic!("Unexpected object type: {:?}", o),
        };
        if let Some(set) = set {
            if set.contains(checksum) {
                return Ok(());
            }
            let inserted = set.insert(checksum.to_string());
            debug_assert!(inserted);
        }

        let mut h = tar::Header::new_gnu();
        h.set_uid(0);
        h.set_gid(0);
        h.set_mode(0o644);
        let data = v.data_as_bytes();
        let data = data.as_ref();
        h.set_size(data.len() as u64);
        self.out
            .append_data(&mut h, &object_path(objtype, checksum), data)
            .with_context(|| format!("Writing object {}", checksum))?;
        Ok(())
    }

    #[context("Writing xattrs")]
    fn append_xattrs(
        &mut self,
        xattrs: &glib::Variant,
    ) -> Result<Option<(Utf8PathBuf, tar::Header)>> {
        let xattrs_data = xattrs.data_as_bytes();
        let xattrs_data = xattrs_data.as_ref();
        if xattrs_data.is_empty() {
            return Ok(None);
        }

        let mut h = tar::Header::new_gnu();
        h.set_mode(0o644);
        h.set_size(0);
        let digest = openssl::hash::hash(openssl::hash::MessageDigest::sha256(), xattrs_data)?;
        let mut hexbuf = [0u8; 64];
        hex::encode_to_slice(digest, &mut hexbuf)?;
        let checksum = std::str::from_utf8(&hexbuf)?;
        let path = xattrs_path(checksum);

        if !self.wrote_xattrs.contains(checksum) {
            let inserted = self.wrote_xattrs.insert(checksum.to_string());
            debug_assert!(inserted);
            let mut target_header = h.clone();
            target_header.set_size(xattrs_data.len() as u64);
            self.out
                .append_data(&mut target_header, &path, xattrs_data)?;
        }
        Ok(Some((path, h)))
    }

    /// Write a content object, returning the path/header that should be used
    /// as a hard link to it in the target path.  This matches how ostree checkouts work.
    fn append_content(&mut self, checksum: &str) -> Result<(Utf8PathBuf, tar::Header)> {
        let path = object_path(ostree::ObjectType::File, checksum);

        let (instream, meta, xattrs) = self.repo.load_file(checksum, gio::NONE_CANCELLABLE)?;
        let meta = meta.unwrap();
        let xattrs = xattrs.unwrap();

        let mut h = tar::Header::new_gnu();
        h.set_uid(meta.attribute_uint32("unix::uid") as u64);
        h.set_gid(meta.attribute_uint32("unix::gid") as u64);
        let mode = meta.attribute_uint32("unix::mode");
        h.set_mode(mode);
        let mut target_header = h.clone();
        target_header.set_size(0);

        if !self.wrote_content.contains(checksum) {
            let inserted = self.wrote_content.insert(checksum.to_string());
            debug_assert!(inserted);

            if let Some((xattrspath, mut xattrsheader)) = self.append_xattrs(&xattrs)? {
                xattrsheader.set_entry_type(tar::EntryType::Link);
                xattrsheader.set_link_name(xattrspath)?;
                let subpath = format!("{}.xattrs", path);
                self.out
                    .append_data(&mut xattrsheader, subpath, &mut std::io::empty())?;
            }

            if let Some(instream) = instream {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_size(meta.size() as u64);
                let mut instream = BufReader::with_capacity(BUF_CAPACITY, instream.into_read());
                self.out
                    .append_data(&mut h, &path, &mut instream)
                    .with_context(|| format!("Writing regfile {}", checksum))?;
            } else {
                h.set_size(0);
                h.set_entry_type(tar::EntryType::Symlink);
                let context = || format!("Writing content symlink: {}", checksum);
                h.set_link_name(meta.symlink_target().unwrap().as_str())
                    .with_context(context)?;
                self.out
                    .append_data(&mut h, &path, &mut std::io::empty())
                    .with_context(context)?;
            }
        }

        Ok((path, target_header))
    }

    /// Write a dirtree object.
    fn append_dirtree<C: IsA<gio::Cancellable>>(
        &mut self,
        dirpath: &Utf8Path,
        checksum: &str,
        cancellable: Option<&C>,
    ) -> Result<()> {
        let v = &self
            .repo
            .load_variant(ostree::ObjectType::DirTree, checksum)?;
        self.append(ostree::ObjectType::DirTree, checksum, v)?;
        let v = v.data_as_bytes();
        let v = v.try_as_aligned()?;
        let v = gv_dirtree!().cast(v);
        let (files, dirs) = v.to_tuple();

        if let Some(c) = cancellable {
            c.set_error_if_cancelled()?;
        }

        // A reusable buffer to avoid heap allocating these
        let mut hexbuf = [0u8; 64];

        for file in files {
            let (name, csum) = file.to_tuple();
            let name = name.to_str();
            hex::encode_to_slice(csum, &mut hexbuf)?;
            let checksum = std::str::from_utf8(&hexbuf)?;
            let (objpath, mut h) = self.append_content(checksum)?;
            h.set_entry_type(tar::EntryType::Link);
            h.set_link_name(&objpath)?;
            let subpath = &dirpath.join(name);
            let subpath = map_path(subpath);
            self.out
                .append_data(&mut h, &*subpath, &mut std::io::empty())?;
        }

        for item in dirs {
            let (name, contents_csum, meta_csum) = item.to_tuple();
            let name = name.to_str();
            {
                hex::encode_to_slice(meta_csum, &mut hexbuf)?;
                let meta_csum = std::str::from_utf8(&hexbuf)?;
                let meta_v = &self
                    .repo
                    .load_variant(ostree::ObjectType::DirMeta, meta_csum)?;
                self.append(ostree::ObjectType::DirMeta, meta_csum, meta_v)?;
            }
            hex::encode_to_slice(contents_csum, &mut hexbuf)?;
            let dirtree_csum = std::str::from_utf8(&hexbuf)?;
            let subpath = &dirpath.join(name);
            let subpath = map_path(subpath);
            self.append_dirtree(&*subpath, dirtree_csum, cancellable)?;
        }

        Ok(())
    }
}

/// Recursively walk an OSTree commit and generate data into a `[tar::Builder]`
/// which contains all of the metadata objects, as well as a hardlinked
/// stream that looks like a checkout.  Extended attributes are stored specially out
/// of band of tar so that they can be reliably retrieved.
fn impl_export<W: std::io::Write>(
    repo: &ostree::Repo,
    commit_checksum: &str,
    out: &mut tar::Builder<W>,
) -> Result<()> {
    let writer = &mut OstreeTarWriter::new(repo, out);
    writer.write_commit(commit_checksum)?;
    Ok(())
}

/// Export an ostree commit to an (uncompressed) tar archive stream.
#[context("Exporting commit")]
pub fn export_commit(repo: &ostree::Repo, rev: &str, out: impl std::io::Write) -> Result<()> {
    let commit = repo.resolve_rev(rev, false)?;
    let mut tar = tar::Builder::new(out);
    impl_export(repo, commit.unwrap().as_str(), &mut tar)?;
    tar.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_path() {
        assert_eq!(map_path("/".into()), Utf8Path::new("/"));
        assert_eq!(
            map_path("./usr/etc/blah".into()),
            Utf8Path::new("./etc/blah")
        );
    }
}
