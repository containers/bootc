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
pub mod refescape;
pub mod tar;
pub mod tokio_util;

mod cmdext;

/// Prelude, intended for glob import.
pub mod prelude {
    #[doc(hidden)]
    pub use ostree::prelude::*;
}
