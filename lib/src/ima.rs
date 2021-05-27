//! Write IMA signatures to an ostree commit

// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::variant_utils;
use anyhow::{Context, Result};
use fn_error_context::context;
use gio::prelude::InputStreamExtManual;
use glib::translate::*;
use glib::Cast;
use gvariant::aligned_bytes::TryAsAligned;
use gvariant::{gv, Marker, Structure};
use openat_ext::FileExt;
use std::collections::{BTreeMap, HashMap};
use std::ffi::CString;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::os::unix::prelude::{FromRawFd, IntoRawFd};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::{convert::TryInto, io::Seek};

/// Extended attribute keys used for IMA.
const IMA_XATTRS: &[&str] = &["security.ima", "security.evm"];
const SELINUX_XATTR: &[u8] = b"security.selinux\0";

/// Attributes to configure IMA signatures.
#[derive(Debug, Clone)]
pub struct ImaOpts {
    /// Digest algorithm
    pub algorithm: String,

    /// Path to IMA key
    pub key: String,
}

/// Convert a GVariant of type `a(ayay)` to a mutable map
fn xattrs_to_map(v: &glib::Variant) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let v = v.get_data_as_bytes();
    let v = v.try_as_aligned().unwrap();
    let v = gv!("a(ayay)").cast(v);
    let mut map: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for e in v.iter() {
        let (k, v) = e.to_tuple();
        map.insert(k.into(), v.into());
    }
    map
}

/// Reserialize a map to GVariant of type `a(ayay)`
fn xattrmap_serialize(map: &BTreeMap<Vec<u8>, Vec<u8>>) -> glib::Variant {
    let map: Vec<_> = map.iter().collect();
    variant_utils::new_variant_a_ayay(&map)
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
            return Err(nix::Error::last().into());
        }
        let sz: usize = r.try_into()?;
        let mut buf = vec![0u8; sz];
        let r = libc::fgetxattr(f.as_raw_fd(), k, buf.as_mut_ptr() as *mut _, sz);
        if r < 0 {
            return Err(nix::Error::last().into());
        }
        let r = libc::fremovexattr(f.as_raw_fd(), k);
        if r < 0 {
            return Err(nix::Error::last().into());
        }
        Ok(buf)
    }
}

impl<'a> CommitRewriter<'a> {
    fn new(repo: &'a ostree::Repo, ima: &'a ImaOpts) -> Result<Self> {
        Ok(Self {
            repo,
            ima,
            tempdir: tempfile::tempdir_in(format!("/proc/self/fd/{}/tmp", repo.get_dfd()))?,
            rewritten_files: Default::default(),
        })
    }

    /// Use `evmctl` to generate an IMA signature on a file, then
    /// scrape the xattr value out of it (removing it).
    ///
    /// evmctl can write a separate file but it picks the name...so
    /// we do this hacky dance of `--xattr-user` instead.
    #[allow(unsafe_code)]
    #[context("Invoking evmctl")]
    fn ima_sign(
        &self,
        instream: &gio::InputStream,
        selinux: Option<&Vec<u8>>,
    ) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
        let mut tempf = tempfile::NamedTempFile::new_in(self.tempdir.path())?;
        // If we're operating on a bare repo, we can clone the file (copy_file_range) directly.
        if let Ok(instream) = instream.clone().downcast::<gio::UnixInputStream>() {
            // View the fd as a File
            let instream_fd = unsafe { File::from_raw_fd(instream.as_raw_fd()) };
            instream_fd.copy_to(tempf.as_file_mut())?;
            // Leak to avoid double close
            let _ = instream_fd.into_raw_fd();
        } else {
            // If we're operating on an archive repo, then we need to uncompress
            // and recompress...
            let mut instream = instream.clone().into_read();
            let _n = std::io::copy(&mut instream, tempf.as_file_mut())?;
        }
        tempf.seek(std::io::SeekFrom::Start(0))?;

        let mut proc = Command::new("evmctl");
        proc.current_dir(self.tempdir.path())
            .args(&[
                "sign",
                "--portable",
                "--xattr-user",
                "--key",
                self.ima.key.as_str(),
            ])
            .args(&["--hashalgo", self.ima.algorithm.as_str()]);
        if let Some(selinux) = selinux {
            let selinux = std::str::from_utf8(selinux)
                .context("Non-UTF8 selinux value")?
                .trim_end_matches('\0');
            proc.args(&["--selinux", selinux]);
        }

        let proc = proc
            .arg("--imasig")
            .arg(tempf.path().file_name().unwrap())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let status = proc.output().context("Spawning evmctl")?;
        if !status.status.success() {
            return Err(anyhow::anyhow!(
                "evmctl failed: {:?}\n{}",
                status.status,
                String::from_utf8_lossy(&status.stderr),
            ));
        }
        let mut r = HashMap::new();
        for &k in IMA_XATTRS {
            let user_k = k.replace("security.", "user.");
            let v = steal_xattr(tempf.as_file(), user_k.as_str())?;
            // NUL terminate the key
            let k = CString::new(k)?.into_bytes_with_nul();
            r.insert(k, v);
        }
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

        let selinux = xattrs.get(SELINUX_XATTR);

        // Now inject the IMA xattr
        let xattrs = {
            let signed = self.ima_sign(&instream, selinux)?;
            xattrs.extend(signed);
            xattrmap_serialize(&xattrs)
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
    #[allow(unsafe_code)]
    fn map_dirtree(&mut self, checksum: &str) -> Result<String> {
        let src = &self
            .repo
            .load_variant(ostree::ObjectType::DirTree, checksum)?;
        let src = src.get_data_as_bytes();
        let src = src.try_as_aligned()?;
        let src = gv!("(a(say)a(sayay))").cast(src);
        let (files, dirs) = src.to_tuple();

        // A reusable buffer to avoid heap allocating these
        let mut hexbuf = [0u8; 64];

        let new_files_builder =
            unsafe { glib_sys::g_variant_builder_new(b"a(say)\0".as_ptr() as *const _) };
        for file in files {
            let (name, csum) = file.to_tuple();
            let name = name.to_str();
            hex::encode_to_slice(csum, &mut hexbuf)?;
            let checksum = std::str::from_utf8(&hexbuf)?;
            let mapped = self.map_file(checksum)?;
            let mapped = hex::decode(&*mapped)?;
            unsafe {
                // Unwrap safety: The name won't have NULs
                let name = CString::new(name).unwrap();
                let mapped_checksum_v = variant_utils::new_variant_bytearray(&mapped);
                let name_p = name.as_ptr();
                glib_sys::g_variant_builder_add(
                    new_files_builder,
                    b"(s@ay)\0".as_ptr() as *const _,
                    name_p,
                    mapped_checksum_v.to_glib_none().0,
                );
            }
        }
        let new_files: glib::Variant = unsafe {
            let v = glib_sys::g_variant_builder_end(new_files_builder);
            glib_sys::g_variant_ref_sink(v);
            from_glib_full(v)
        };

        let new_dirs_builder =
            unsafe { glib_sys::g_variant_builder_new(b"a(sayay)\0".as_ptr() as *const _) };
        for item in dirs {
            let (name, contents_csum, meta_csum_bytes) = item.to_tuple();
            let name = name.to_str();
            hex::encode_to_slice(contents_csum, &mut hexbuf)?;
            let contents_csum = std::str::from_utf8(&hexbuf)?;
            let mapped = self.map_dirtree(&contents_csum)?;
            let mapped = hex::decode(mapped)?;
            unsafe {
                // Unwrap safety: The name won't have NULs
                let name = CString::new(name).unwrap();
                let mapped_checksum_v = variant_utils::new_variant_bytearray(&mapped);
                let meta_checksum_v = variant_utils::new_variant_bytearray(meta_csum_bytes);
                glib_sys::g_variant_builder_add(
                    new_dirs_builder,
                    b"(s@ay@ay)\0".as_ptr() as *const _,
                    name.as_ptr(),
                    mapped_checksum_v.to_glib_none().0,
                    meta_checksum_v.to_glib_none().0,
                );
            }
        }
        let new_dirs: glib::Variant = unsafe {
            let v = glib_sys::g_variant_builder_end(new_dirs_builder);
            glib_sys::g_variant_ref_sink(v);
            from_glib_full(v)
        };

        let new_dirtree: glib::Variant = unsafe {
            let v = glib_sys::g_variant_new(
                b"(@a(say)@a(sayay))\0".as_ptr() as *const _,
                new_files.to_glib_none().0,
                new_dirs.to_glib_none().0,
                std::ptr::null_mut::<libc::c_char>(),
            );
            glib_sys::g_variant_ref_sink(v);
            from_glib_full(v)
        };

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
        let checksum = self.repo.resolve_rev(rev, false)?.unwrap();
        let cancellable = gio::NONE_CANCELLABLE;
        let (commit_v, _) = self.repo.load_commit(&checksum)?;
        let commit_v = &commit_v;

        let commit_bytes = commit_v.get_data_as_bytes();
        let commit_bytes = commit_bytes.try_as_aligned()?;
        let commit = gv!("(a{sv}aya(say)sstayay)").cast(commit_bytes);
        let commit = commit.to_tuple();
        let contents = &hex::encode(commit.6);

        let new_dt = self.map_dirtree(contents)?;

        let n_parts = 8;
        let mut parts = Vec::with_capacity(n_parts);
        for i in 0..n_parts {
            parts.push(variant_utils::variant_get_child_value(&commit_v, i).unwrap());
        }
        let new_dt = hex::decode(new_dt)?;
        parts[6] = variant_utils::new_variant_bytearray(&new_dt);
        let new_commit = variant_utils::new_variant_tuple(&parts);

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
    let writer = &mut CommitRewriter::new(&repo, &opts)?;
    writer.map_commit(ostree_ref)
}
