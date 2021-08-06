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

/// Our generic catchall fatal error, expected to be converted
/// to a string to output to a terminal or logs.
type Result<T> = anyhow::Result<T>;

mod async_util;
pub mod cli;
pub mod container;
pub mod diff;
pub mod ima;
pub mod tar;
#[allow(unsafe_code)]
pub mod variant_utils;
