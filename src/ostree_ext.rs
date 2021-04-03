//! Extension traits fixing incorrectly bound things in ostree-rs
//! by defining a new function with an `x_` prefix.

// SPDX-License-Identifier: Apache-2.0 OR MIT

use glib::translate::*;
use std::ptr;

/// Extension functions which fix incorrectly bound APIs.
pub trait RepoExt {
    fn x_load_variant_if_exists(
        &self,
        objtype: ostree::ObjectType,
        checksum: &str,
    ) -> Result<Option<glib::Variant>, glib::Error>;
}

impl RepoExt for ostree::Repo {
    #[allow(unsafe_code)]
    fn x_load_variant_if_exists(
        &self,
        objtype: ostree::ObjectType,
        checksum: &str,
    ) -> Result<Option<glib::Variant>, glib::Error> {
        unsafe {
            let mut out_v = ptr::null_mut();
            let mut error = ptr::null_mut();
            let checksum = checksum.to_glib_none();
            let _ = ostree_sys::ostree_repo_load_variant_if_exists(
                self.to_glib_none().0,
                objtype.to_glib(),
                checksum.0,
                &mut out_v,
                &mut error,
            );
            if error.is_null() {
                Ok(from_glib_full(out_v))
            } else {
                Err(from_glib_full(error))
            }
        }
    }
}
