//! # Extension APIs for ostree
//!
//! This crate builds on top of the core ostree C library
//! and the Rust bindings to it, adding new functionality
//! written in Rust.  

#![deny(missing_docs)]
// Good defaults
#![forbid(unused_must_use)]
#![deny(unsafe_code)]

/// Our generic catchall fatal error, expected to be converted
/// to a string to output to a terminal or logs.
type Result<T> = anyhow::Result<T>;

pub mod container;
pub mod diff;
pub mod ostree_ext;
pub mod tar;
mod variant_utils;
