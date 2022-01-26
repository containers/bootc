//! Global functions.

use super::Result;
use cap_std_ext::rustix;
use once_cell::sync::OnceCell;
use ostree::glib;
use std::fs::File;
use std::path::{Path, PathBuf};

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
        if let Some(f) = crate::container_utils::open_optional(&runtime)? {
            return Ok(Some((runtime, f)));
        }
        let mut persistent = self.persistent.clone();
        persistent.push(p);
        if let Some(f) = crate::container_utils::open_optional(&persistent)? {
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
