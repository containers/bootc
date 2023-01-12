use std::collections::HashSet;

use anyhow::{bail, Context, Result};
use openat_ext::OpenatDirExt;

use std::path::Path;
use std::process::Command;

use crate::model::*;
use crate::ostreeutil;
use chrono::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) trait CommandRunExt {
    fn run(&mut self) -> Result<()>;
}

impl CommandRunExt for Command {
    fn run(&mut self) -> Result<()> {
        let r = self.status()?;
        if !r.success() {
            bail!("Child [{:?}] exited: {}", self, r);
        }
        Ok(())
    }
}

/// Parse an environment variable as UTF-8
#[allow(dead_code)]
pub(crate) fn getenv_utf8(n: &str) -> Result<Option<String>> {
    if let Some(v) = std::env::var_os(n) {
        Ok(Some(
            v.to_str()
                .ok_or_else(|| anyhow::anyhow!("{} is invalid UTF-8", n))?
                .to_string(),
        ))
    } else {
        Ok(None)
    }
}

pub(crate) fn filenames(dir: &openat::Dir) -> Result<HashSet<String>> {
    let mut ret = HashSet::new();
    for entry in dir.list_dir(".")? {
        let entry = entry?;
        let name = if let Some(name) = entry.file_name().to_str() {
            name
        } else {
            bail!("Invalid UTF-8 filename: {:?}", entry.file_name())
        };
        match dir.get_file_type(&entry)? {
            openat::SimpleType::File => {
                ret.insert(format!("/{name}"));
            }
            openat::SimpleType::Dir => {
                let child = dir.sub_dir(name)?;
                for mut k in filenames(&child)?.drain() {
                    k.reserve(name.len() + 1);
                    k.insert_str(0, name);
                    k.insert(0, '/');
                    ret.insert(k);
                }
            }
            openat::SimpleType::Symlink => {
                bail!("Unsupported symbolic link {:?}", entry.file_name())
            }
            openat::SimpleType::Other => {
                bail!("Unsupported non-file/directory {:?}", entry.file_name())
            }
        }
    }
    Ok(ret)
}

pub(crate) fn ensure_writable_mount<P: AsRef<Path>>(p: P) -> Result<()> {
    use nix::sys::statvfs;
    let p = p.as_ref();
    let stat = statvfs::statvfs(p)?;
    if !stat.flags().contains(statvfs::FsFlags::ST_RDONLY) {
        return Ok(());
    }
    let status = std::process::Command::new("mount")
        .args(&["-o", "remount,rw"])
        .arg(p)
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to remount {:?} writable", p);
    }
    Ok(())
}

/// Parse the output of `rpm -q`
pub(crate) fn parse_rpm_metadata(stdout: Vec<u8>) -> Result<ContentMetadata> {
    let pkgs = std::str::from_utf8(&stdout)?
        .split_whitespace()
        .map(|s| -> Result<_> {
            let parts: Vec<_> = s.splitn(2, ',').collect();
            let name = parts[0];
            if let Some(ts) = parts.get(1) {
                let nt = NaiveDateTime::parse_from_str(ts, "%s")
                    .context("Failed to parse rpm buildtime")?;
                Ok((name, DateTime::<Utc>::from_utc(nt, Utc)))
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
pub(crate) fn rpm_query(sysroot_path: &str, path: &Path) -> Result<Command> {
    let mut c = ostreeutil::rpm_cmd(sysroot_path);
    c.args(&["-q", "--queryformat", "%{nevra},%{buildtime} ", "-f"]);

    match path.file_name().expect("filename").to_str() {
        Some("EFI") => {
            let efidir = openat::Dir::open(path)?;
            c.args(filenames(&efidir)?.drain().map(|mut f| {
                f.insert_str(0, "/boot/efi/EFI/");
                f
            }));
        }
        _ => {
            bail!("Unsupported file/directory {:?}", path)
        }
    }
    Ok(c)
}
