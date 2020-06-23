/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{bail, Result};

/// The path to the ESP mount
pub(crate) const MOUNT_PATH: &str = "boot/efi";

pub(crate) fn validate_esp<P: AsRef<std::path::Path>>(mnt: P) -> Result<()> {
    if crate::util::running_in_test_suite() {
        return Ok(());
    }
    let mnt = mnt.as_ref();
    let stat = nix::sys::statfs::statfs(mnt)?;
    let fstype = stat.filesystem_type();
    if fstype != nix::sys::statfs::MSDOS_SUPER_MAGIC {
        bail!(
            "Mount {} is not a msdos filesystem, but is {:?}",
            mnt.display(),
            fstype
        );
    };
    Ok(())
}
