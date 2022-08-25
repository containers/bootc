//! Manual workarounds for ostree bugs

use std::io::Read;
use std::ptr;

use ostree;
use ostree::prelude::{Cast, InputStreamExtManual};
use ostree::{gio, glib};

#[allow(unsafe_code)]

/// Equivalent of `g_file_read()` for ostree::RepoFile to work around an ostree bug.
pub fn repo_file_read(f: &ostree::RepoFile) -> Result<gio::InputStream, glib::Error> {
    use glib::translate::*;
    let stream = unsafe {
        let f = f.upcast_ref::<gio::File>();
        let mut error = ptr::null_mut();
        let stream = gio::ffi::g_file_read(f.to_glib_none().0, ptr::null_mut(), &mut error);
        if !error.is_null() {
            return Err(from_glib_full(error));
        }
        let stream = stream as *mut gio::ffi::GInputStream;
        from_glib_full(stream)
    };

    Ok(stream)
}

/// Read a repo file to a string.
pub fn repo_file_read_to_string(f: &ostree::RepoFile) -> anyhow::Result<String> {
    let mut r = String::new();
    let mut s = repo_file_read(f)?.into_read();
    s.read_to_string(&mut r)?;
    Ok(r)
}
