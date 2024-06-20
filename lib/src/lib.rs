//! # Bootable container tool
//!
//! This crate builds on top of ostree's container functionality
//! to provide a fully "container native" tool for using
//! bootable container images.

// See https://doc.rust-lang.org/rustc/lints/listing/allowed-by-default.html
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![forbid(unused_must_use)]
#![deny(unsafe_code)]
#![cfg_attr(feature = "dox", feature(doc_cfg))]
#![deny(clippy::dbg_macro)]
#![deny(clippy::todo)]
// These two are in my experience the lints which are most likely
// to trigger, and among the least valuable to fix.
#![allow(clippy::needless_borrow)]
#![allow(clippy::needless_borrows_for_generic_args)]

pub mod cli;
pub(crate) mod deploy;
pub(crate) mod generator;
mod image;
pub(crate) mod journal;
pub(crate) mod kargs;
mod lints;
mod lsm;
pub(crate) mod metadata;
mod reboot;
mod reexec;
mod status;
mod task;
mod utils;

#[cfg(feature = "install")]
mod blockdev;
#[cfg(feature = "install")]
mod bootloader;
#[cfg(feature = "install")]
mod containerenv;
#[cfg(feature = "install")]
mod install;
mod k8sapitypes;
#[cfg(feature = "install")]
mod kernel;
#[cfg(feature = "install")]
pub(crate) mod mount;
#[cfg(feature = "install")]
mod podman;
pub mod spec;

#[cfg(feature = "docgen")]
mod docgen;
