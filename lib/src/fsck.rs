//! # Write deployments merging image with configmap
//!
//! Create a merged filesystem tree with the image and mounted configmaps.

// Unfortunately needed here to work with linkme
#![allow(unsafe_code)]

use std::process::Command;

use cap_std_ext::cap_std;
use cap_std_ext::cap_std::fs::{Dir, MetadataExt as _};
use cap_std_ext::dirext::CapStdExtDirExt;
use linkme::distributed_slice;

use crate::store::Storage;

/// A lint check has failed.
#[derive(thiserror::Error, Debug)]
struct FsckError(String);

/// The outer error is for unexpected fatal runtime problems; the
/// inner error is for the check failing in an expected way.
type FsckResult = anyhow::Result<std::result::Result<(), FsckError>>;

/// Everything is OK - we didn't encounter a runtime error, and
/// the targeted check passed.
fn fsck_ok() -> FsckResult {
    Ok(Ok(()))
}

/// We successfully found a failure.
fn fsck_err(msg: impl AsRef<str>) -> FsckResult {
    Ok(Err(FsckError::new(msg)))
}

impl std::fmt::Display for FsckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FsckError {
    fn new(msg: impl AsRef<str>) -> Self {
        Self(msg.as_ref().to_owned())
    }
}

#[derive(Debug)]
struct FsckCheck {
    name: &'static str,
    ordering: u16,
    f: FsckFn,
}

type FsckFn = fn(&Storage) -> FsckResult;
#[distributed_slice]
pub(crate) static FSCK_CHECKS: [FsckCheck];

impl FsckCheck {
    pub(crate) const fn new(name: &'static str, ordering: u16, f: FsckFn) -> Self {
        FsckCheck { name, ordering, f }
    }
}

#[distributed_slice(FSCK_CHECKS)]
static CHECK_RESOLVCONF: FsckCheck = FsckCheck::new("etc-resolvconf", 5, check_resolvconf);
/// See https://github.com/containers/bootc/pull/1096 and https://github.com/containers/bootc/pull/1167
/// Basically verify that if /usr/etc/resolv.conf exists, it is not a zero-sized file that was
/// probably injected by buildah and that bootc should have removed.
///
/// Note that this fsck check can fail for systems upgraded from old bootc right now, as
/// we need the *new* bootc to fix it.
///
/// But at the current time fsck is an experimental feature that we should only be running
/// in our CI.
fn check_resolvconf(storage: &Storage) -> FsckResult {
    // For now we only check the booted deployment.
    if storage.booted_deployment().is_none() {
        return fsck_ok();
    }
    // Read usr/etc/resolv.conf directly.
    let usr = Dir::open_ambient_dir("/usr", cap_std::ambient_authority())?;
    let Some(meta) = usr.symlink_metadata_optional("etc/resolv.conf")? else {
        return fsck_ok();
    };
    if meta.is_file() && meta.size() == 0 {
        return fsck_err("Found usr/etc/resolv.conf as zero-sized file");
    }
    fsck_ok()
}

pub(crate) fn fsck(storage: &Storage, mut output: impl std::io::Write) -> anyhow::Result<()> {
    let mut checks = FSCK_CHECKS.static_slice().iter().collect::<Vec<_>>();
    checks.sort_by(|a, b| a.ordering.cmp(&b.ordering));

    let mut errors = false;
    for check in checks.iter() {
        let name = check.name;
        // SAFETY: Shouldn't fail to join a thread
        match (check.f)(&storage) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                errors = true;
                writeln!(output, "Failed check \"{name}\": {e}")?;
            }
            Err(e) => {
                errors = true;
                writeln!(output, "Unexpected runtime error in check \"{name}\": {e}")?;
            }
        }
        println!("ok: {name}");
    }
    if errors {
        anyhow::bail!("Encountered errors")
    }

    // Run an `ostree fsck` (yes, ostree exposes enough APIs
    // that we could reimplement this in Rust, but eh)
    let st = Command::new("ostree")
        .arg("fsck")
        .stdin(std::process::Stdio::inherit())
        .status()?;
    if !st.success() {
        anyhow::bail!("ostree fsck failed");
    }

    Ok(())
}
