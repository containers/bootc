use std::collections::HashSet;

use anyhow::{bail, Result};
use openat_ext::OpenatDirExt;

use std::process::Command;

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
                ret.insert(format!("/{}", name));
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
