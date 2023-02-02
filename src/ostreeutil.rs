/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use std::path::Path;

/// https://github.com/coreos/rpm-ostree/pull/969/commits/dc0e8db5bd92e1f478a0763d1a02b48e57022b59
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub(crate) const BOOT_PREFIX: &str = "usr/lib/ostree-boot";

pub(crate) fn rpm_cmd<P: AsRef<Path>>(sysroot: P) -> std::process::Command {
    let sysroot = sysroot.as_ref();
    let dbpath = sysroot.join("usr/share/rpm");
    let dbpath_arg = {
        let mut s = std::ffi::OsString::new();
        s.push("--dbpath=");
        s.push(dbpath.as_os_str());
        s
    };
    let mut c = std::process::Command::new("rpm");
    c.arg(&dbpath_arg);
    c
}
