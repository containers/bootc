//! # Extension APIs for ostree
//!
//! This crate builds on top of the core ostree C library
//! and the Rust bindings to it, adding new functionality
//! written in Rust.  

#![deny(missing_docs)]
// Good defaults
#![forbid(unused_must_use)]
#![deny(unsafe_code)]
#![cfg_attr(feature = "dox", feature(doc_cfg))]

// Re-export our dependencies.  See https://gtk-rs.org/blog/2021/06/22/new-release.html
// "Dependencies are re-exported".  Users will need e.g. `gio::File`, so this avoids
// them needing to update matching versions.
pub use ostree;
pub use ostree::gio;
pub use ostree::gio::glib;

/// Our generic catchall fatal error, expected to be converted
/// to a string to output to a terminal or logs.
type Result<T> = anyhow::Result<T>;

mod async_util;
pub mod cli;
pub mod container;
pub mod diff;
pub mod ima;
pub mod tar;
/// Prelude, intended for glob import.
pub mod prelude {
    #[doc(hidden)]
    pub use ostree::prelude::*;
}

/// Temporary holding place for fixed APIs
#[allow(unsafe_code)]
mod ostree_ffi_fixed {
    use super::*;
    use ostree::prelude::*;

    /// https://github.com/ostreedev/ostree/pull/2422
    pub(crate) fn read_commit_detached_metadata<P: IsA<gio::Cancellable>>(
        repo: &ostree::Repo,
        checksum: &str,
        cancellable: Option<&P>,
    ) -> std::result::Result<Option<glib::Variant>, glib::Error> {
        use glib::translate::*;
        use std::ptr;
        unsafe {
            let mut out_metadata = ptr::null_mut();
            let mut error = ptr::null_mut();
            let _ = ostree::ffi::ostree_repo_read_commit_detached_metadata(
                repo.to_glib_none().0,
                checksum.to_glib_none().0,
                &mut out_metadata,
                cancellable.map(|p| p.as_ref()).to_glib_none().0,
                &mut error,
            );
            if error.is_null() {
                Ok(from_glib_full(out_metadata))
            } else {
                Err(from_glib_full(error))
            }
        }
    }
}
