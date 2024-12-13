//! # Bootable container tool
//!
//! This crate builds on top of ostree's container functionality
//! to provide a fully "container native" tool for using
//! bootable container images.

mod boundimage;
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
mod store;
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
mod podman;
pub mod spec;

#[cfg(feature = "docgen")]
mod docgen;
mod glyph;
mod imgstorage;
mod progress_jsonl;
