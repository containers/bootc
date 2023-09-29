use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::prelude::*;

use crate::model::*;
use crate::ostreeutil;

/// Parse the output of `rpm -q`
pub(crate) fn parse_metadata(stdout: Vec<u8>) -> Result<ContentMetadata> {
    let pkgs = std::str::from_utf8(&stdout)?
        .split_whitespace()
        .map(|s| -> Result<_> {
            let parts: Vec<_> = s.splitn(2, ',').collect();
            let name = parts[0];
            if let Some(ts) = parts.get(1) {
                let nt = DateTime::parse_from_str(ts, "%s")
                    .context("Failed to parse rpm buildtime")?
                    .with_timezone(&chrono::Utc);
                Ok((name, nt))
            } else {
                bail!("Failed to parse: {}", s);
            }
        })
        .collect::<Result<BTreeMap<&str, DateTime<Utc>>>>()?;
    if pkgs.is_empty() {
        bail!("Failed to find any RPM packages matching files in source efidir");
    }
    let timestamps: BTreeSet<&DateTime<Utc>> = pkgs.values().collect();
    // Unwrap safety: We validated pkgs has at least one value above
    let largest_timestamp = timestamps.iter().last().unwrap();
    let version = pkgs.keys().fold("".to_string(), |mut s, n| {
        if !s.is_empty() {
            s.push(',');
        }
        s.push_str(n);
        s
    });
    Ok(ContentMetadata {
        timestamp: **largest_timestamp,
        version,
    })
}

/// Query the rpm database and list the package and build times, for all the
/// files in the EFI system partition, or for grub2-install file
pub(crate) fn query(sysroot_path: &str, path: &Path) -> Result<Command> {
    let mut c = ostreeutil::rpm_cmd(sysroot_path);
    c.args(["-q", "--queryformat", "%{nevra},%{buildtime} ", "-f"]);

    match path.file_name().expect("filename").to_str() {
        Some("EFI") => {
            let efidir = openat::Dir::open(path)?;
            c.args(crate::util::filenames(&efidir)?.drain().map(|mut f| {
                f.insert_str(0, "/boot/efi/EFI/");
                f
            }));
        }
        Some("grub2-install") => {
            c.arg(path);
        }
        _ => {
            bail!("Unsupported file/directory {:?}", path)
        }
    }
    Ok(c)
}
