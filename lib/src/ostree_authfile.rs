//! # Copy of the ostree authfile bits as they're not public

use anyhow::Result;
use once_cell::sync::OnceCell;
use ostree_ext::glib;
use std::fs::File;
use std::path::{Path, PathBuf};

// https://docs.rs/openat-ext/0.1.10/openat_ext/trait.OpenatDirExt.html#tymethod.open_file_optional
// https://users.rust-lang.org/t/why-i-use-anyhow-error-even-in-libraries/68592
pub(crate) fn open_optional(path: impl AsRef<Path>) -> std::io::Result<Option<std::fs::File>> {
    match std::fs::File::open(path.as_ref()) {
        Ok(r) => Ok(Some(r)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

struct ConfigPaths {
    persistent: PathBuf,
    runtime: PathBuf,
}

/// Get the runtime and persistent config directories.  In the system (root) case, these
/// system(root) case:  /run/ostree           /etc/ostree
/// user(nonroot) case: /run/user/$uid/ostree ~/.config/ostree
fn get_config_paths() -> &'static ConfigPaths {
    static PATHS: OnceCell<ConfigPaths> = OnceCell::new();
    PATHS.get_or_init(|| {
        let mut r = if rustix::process::getuid() == rustix::process::Uid::ROOT {
            ConfigPaths {
                persistent: PathBuf::from("/etc"),
                runtime: PathBuf::from("/run"),
            }
        } else {
            ConfigPaths {
                persistent: glib::user_config_dir(),
                runtime: glib::user_runtime_dir(),
            }
        };
        let path = "ostree";
        r.persistent.push(path);
        r.runtime.push(path);
        r
    })
}

impl ConfigPaths {
    /// Return the path and an open fd for a config file, if it exists.
    pub(crate) fn open_file(&self, p: impl AsRef<Path>) -> Result<Option<(PathBuf, File)>> {
        let p = p.as_ref();
        let mut runtime = self.runtime.clone();
        runtime.push(p);
        if let Some(f) = open_optional(&runtime)? {
            return Ok(Some((runtime, f)));
        }
        let mut persistent = self.persistent.clone();
        persistent.push(p);
        if let Some(f) = open_optional(&persistent)? {
            return Ok(Some((persistent, f)));
        }
        Ok(None)
    }
}

/// Return the path to the global container authentication file, if it exists.
pub(crate) fn get_global_authfile_path() -> Result<Option<PathBuf>> {
    let paths = get_config_paths();
    let r = paths.open_file("auth.json")?;
    // TODO pass the file descriptor to the proxy, not a global path
    Ok(r.map(|v| v.0))
}
