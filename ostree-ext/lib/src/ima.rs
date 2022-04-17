//! Write IMA signatures to an ostree commit

// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::objgv::*;
use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use cap_std_ext::rustix::fd::BorrowedFd;
use fn_error_context::context;
use gio::glib;
use gio::prelude::*;
use glib::Cast;
use glib::Variant;
use gvariant::aligned_bytes::TryAsAligned;
use gvariant::{gv, Marker, Structure};
use io_lifetimes::AsFilelike;
use ostree::gio;
use std::collections::{BTreeMap, HashMap};
use std::ffi::CString;
use std::fs::File;
use std::ops::DerefMut;
use std::os::unix::io::AsRawFd;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::{convert::TryInto, io::Seek};

/// Extended attribute keys used for IMA.
const IMA_XATTR: &str = "security.ima";
const IMA_XATTR_C: &[u8] = b"security.ima\0";

/// Attributes to configure IMA signatures.
#[derive(Debug, Clone)]
pub struct ImaOpts {
    /// Digest algorithm
    pub algorithm: String,

    /// Path to IMA key
    pub key: Utf8PathBuf,

    /// Replace any existing IMA signatures.
    pub overwrite: bool,
}

/// Convert a GVariant of type `a(ayay)` to a mutable map
fn xattrs_to_map(v: &glib::Variant) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let v = v.data_as_bytes();
    let v = v.try_as_aligned().unwrap();
    let v = gv!("a(ayay)").cast(v);
    let mut map: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for e in v.iter() {
        let (k, v) = e.to_tuple();
        map.insert(k.into(), v.into());
    }
    map
}

/// Create a new GVariant of type a(ayay).  This is used by OSTree's extended attributes.
pub(crate) fn new_variant_a_ayay<'a, T: 'a + AsRef<[u8]>>(
    items: impl IntoIterator<Item = (T, T)>,
) -> glib::Variant {
    let children: Vec<_> = items
        .into_iter()
        .map(|(a, b)| {
            let a = a.as_ref();
            let b = b.as_ref();
            Variant::from_tuple(&[a.to_variant(), b.to_variant()])
        })
        .collect();
    Variant::from_array::<(&[u8], &[u8])>(&children)
}

struct CommitRewriter<'a> {
    repo: &'a ostree::Repo,
    ima: &'a ImaOpts,
    tempdir: tempfile::TempDir,
    /// Files that we already changed
    rewritten_files: HashMap<String, Rc<str>>,
}

#[allow(unsafe_code)]
#[context("Gathering xattr {}", k)]
fn steal_xattr(f: &File, k: &str) -> Result<Vec<u8>> {
    let k = &CString::new(k)?;
    unsafe {
        let k = k.as_ptr() as *const _;
        let r = libc::fgetxattr(f.as_raw_fd(), k, std::ptr::null_mut(), 0);
        if r < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let sz: usize = r.try_into()?;
        let mut buf = vec![0u8; sz];
        let r = libc::fgetxattr(f.as_raw_fd(), k, buf.as_mut_ptr() as *mut _, sz);
        if r < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let r = libc::fremovexattr(f.as_raw_fd(), k);
        if r < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(buf)
    }
}

impl<'a> CommitRewriter<'a> {
    fn new(repo: &'a ostree::Repo, ima: &'a ImaOpts) -> Result<Self> {
        Ok(Self {
            repo,
            ima,
            tempdir: tempfile::tempdir_in(format!("/proc/self/fd/{}/tmp", repo.dfd()))?,
            rewritten_files: Default::default(),
        })
    }

    /// Use `evmctl` to generate an IMA signature on a file, then
    /// scrape the xattr value out of it (removing it).
    ///
    /// evmctl can write a separate file but it picks the name...so
    /// we do this hacky dance of `--xattr-user` instead.
    #[allow(unsafe_code)]
    #[context("IMA signing object")]
    fn ima_sign(&self, instream: &gio::InputStream) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
        let mut tempf = tempfile::NamedTempFile::new_in(self.tempdir.path())?;
        // If we're operating on a bare repo, we can clone the file (copy_file_range) directly.
        if let Ok(instream) = instream.clone().downcast::<gio::UnixInputStream>() {
            // View the fd as a File
            let instream_fd = unsafe { BorrowedFd::borrow_raw_fd(instream.as_raw_fd()) };
            let instream_fd = &mut instream_fd.as_filelike_view::<File>();
            std::io::copy(instream_fd.deref_mut(), tempf.as_file_mut())?;
        } else {
            // If we're operating on an archive repo, then we need to uncompress
            // and recompress...
            let mut instream = instream.clone().into_read();
            let _n = std::io::copy(&mut instream, tempf.as_file_mut())?;
        }
        tempf.seek(std::io::SeekFrom::Start(0))?;

        let mut proc = Command::new("evmctl");
        proc.current_dir(self.tempdir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .args(&["ima_sign", "--xattr-user", "--key", self.ima.key.as_str()])
            .args(&["--hashalgo", self.ima.algorithm.as_str()])
            .arg(tempf.path().file_name().unwrap());
        let status = proc.output().context("Spawning evmctl")?;
        if !status.status.success() {
            return Err(anyhow::anyhow!(
                "evmctl failed: {:?}\n{}",
                status.status,
                String::from_utf8_lossy(&status.stderr),
            ));
        }
        let mut r = HashMap::new();
        let user_k = IMA_XATTR.replace("security.", "user.");
        let v = steal_xattr(tempf.as_file(), user_k.as_str())?;
        // NUL terminate the key
        let k = CString::new(IMA_XATTR)?.into_bytes_with_nul();
        r.insert(k, v);
        Ok(r)
    }

    #[context("Content object {}", checksum)]
    fn map_file(&mut self, checksum: &str) -> Result<Rc<str>> {
        if let Some(r) = self.rewritten_files.get(checksum) {
            return Ok(Rc::clone(r));
        }
        let cancellable = gio::NONE_CANCELLABLE;
        let (instream, meta, xattrs) = self.repo.load_file(checksum, cancellable)?;
        let instream = if let Some(i) = instream {
            i
        } else {
            // If there's no input stream, it must be a symlink.  Skip it.
            let r: Rc<str> = checksum.into();
            self.rewritten_files
                .insert(checksum.to_string(), Rc::clone(&r));
            return Ok(r);
        };
        let meta = meta.unwrap();
        let mut xattrs = xattrs_to_map(&xattrs.unwrap());
        let existing_sig = xattrs.remove(IMA_XATTR_C);
        if existing_sig.is_some() && !self.ima.overwrite {
            let r: Rc<str> = checksum.into();
            self.rewritten_files
                .insert(checksum.to_string(), Rc::clone(&r));
            return Ok(r);
        }

        // Now inject the IMA xattr
        let xattrs = {
            let signed = self.ima_sign(&instream)?;
            xattrs.extend(signed);
            new_variant_a_ayay(&xattrs)
        };
        // Now reload the input stream
        let (instream, _, _) = self.repo.load_file(checksum, cancellable)?;
        let instream = instream.unwrap();
        let (ostream, size) =
            ostree::raw_file_to_content_stream(&instream, &meta, Some(&xattrs), cancellable)?;
        let new_checksum = self
            .repo
            .write_content(None, &ostream, size, cancellable)?
            .to_hex();

        let r: Rc<str> = new_checksum.into();
        self.rewritten_files
            .insert(checksum.to_string(), Rc::clone(&r));
        Ok(r)
    }

    /// Write a dirtree object.
    fn map_dirtree(&mut self, checksum: &str) -> Result<String> {
        let src = &self
            .repo
            .load_variant(ostree::ObjectType::DirTree, checksum)?;
        let src = src.data_as_bytes();
        let src = src.try_as_aligned()?;
        let src = gv_dirtree!().cast(src);
        let (files, dirs) = src.to_tuple();

        // A reusable buffer to avoid heap allocating these
        let mut hexbuf = [0u8; 64];

        let mut new_files = Vec::new();
        for file in files {
            let (name, csum) = file.to_tuple();
            let name = name.to_str();
            hex::encode_to_slice(csum, &mut hexbuf)?;
            let checksum = std::str::from_utf8(&hexbuf)?;
            let mapped = self.map_file(checksum)?;
            let mapped = hex::decode(&*mapped)?;
            new_files.push((name, mapped));
        }

        let mut new_dirs = Vec::new();
        for item in dirs {
            let (name, contents_csum, meta_csum_bytes) = item.to_tuple();
            let name = name.to_str();
            hex::encode_to_slice(contents_csum, &mut hexbuf)?;
            let contents_csum = std::str::from_utf8(&hexbuf)?;
            let mapped = self.map_dirtree(contents_csum)?;
            let mapped = hex::decode(mapped)?;
            new_dirs.push((name, mapped, meta_csum_bytes));
        }

        let new_dirtree = (new_files, new_dirs).to_variant();

        let mapped = self
            .repo
            .write_metadata(
                ostree::ObjectType::DirTree,
                None,
                &new_dirtree,
                gio::NONE_CANCELLABLE,
            )?
            .to_hex();

        Ok(mapped)
    }

    /// Write a commit object.
    #[context("Mapping {}", rev)]
    fn map_commit(&mut self, rev: &str) -> Result<String> {
        let checksum = self.repo.require_rev(rev)?;
        let cancellable = gio::NONE_CANCELLABLE;
        let (commit_v, _) = self.repo.load_commit(&checksum)?;
        let commit_v = &commit_v;

        let commit_bytes = commit_v.data_as_bytes();
        let commit_bytes = commit_bytes.try_as_aligned()?;
        let commit = gv_commit!().cast(commit_bytes);
        let commit = commit.to_tuple();
        let contents = &hex::encode(commit.6);

        let new_dt = self.map_dirtree(contents)?;

        let n_parts = 8;
        let mut parts = Vec::with_capacity(n_parts);
        for i in 0..n_parts {
            parts.push(commit_v.child_value(i));
        }
        let new_dt = hex::decode(new_dt)?;
        parts[6] = new_dt.to_variant();
        let new_commit = Variant::from_tuple(&parts);

        let new_commit_checksum = self
            .repo
            .write_metadata(ostree::ObjectType::Commit, None, &new_commit, cancellable)?
            .to_hex();

        Ok(new_commit_checksum)
    }
}

/// Given an OSTree commit and an IMA configuration, generate a new commit object with IMA signatures.
///
/// The generated commit object will inherit all metadata from the existing commit object
/// such as version, etc.
pub fn ima_sign(repo: &ostree::Repo, ostree_ref: &str, opts: &ImaOpts) -> Result<String> {
    let writer = &mut CommitRewriter::new(repo, opts)?;
    writer.map_commit(ostree_ref)
}
