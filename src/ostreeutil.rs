/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{bail, Result};
use std::io::BufRead;
use std::path::Path;

pub(crate) fn find_deployed_commit(sysroot_path: &str) -> Result<String> {
    // ostree_sysroot_get_deployments() isn't bound
    // https://gitlab.com/fkrull/ostree-rs/-/issues/3
    let ls = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("ls -d {}/ostree/deploy/*/deploy/*.0", sysroot_path))
        .output()?;
    if !ls.status.success() {
        bail!("failed to find deployment")
    }
    let mut lines = ls.stdout.lines();
    let deployment = if let Some(line) = lines.next() {
        let line = line?;
        let deploypath = Path::new(line.trim());
        let parts: Vec<_> = deploypath
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .splitn(2, ".0")
            .collect();
        assert!(parts.len() == 2);
        parts[0].to_string()
    } else {
        bail!("failed to find deployment");
    };
    if lines.next().is_some() {
        bail!("multiple deployments found")
    }
    Ok(deployment)
}
