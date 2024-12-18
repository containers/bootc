//! # Bootable container tool
//!
//! This crate builds on top of ostree's container functionality
//! to provide a fully "container native" tool for using
//! bootable container images.

mod boundimage;
pub mod cli;
pub(crate) mod deploy;
pub(crate) mod generator;
mod glyph;
mod image;
mod imgstorage;
pub(crate) mod journal;
mod k8sapitypes;
pub(crate) mod kargs;
mod lints;
mod lsm;
pub(crate) mod metadata;
mod podman;
mod progress_jsonl;
mod reboot;
mod reexec;
pub mod spec;
mod status;
mod store;
mod task;
mod utils;

#[cfg(feature = "docgen")]
mod docgen;

#[cfg(feature = "install")]
mod blockdev;
#[cfg(feature = "install")]
mod bootloader;
#[cfg(feature = "install")]
mod containerenv;
#[cfg(feature = "install")]
mod install;
#[cfg(feature = "install")]
mod kernel;
#[cfg(feature = "install")]
pub(crate) mod mount;

#[cfg(feature = "rhsm")]
mod rhsm;
