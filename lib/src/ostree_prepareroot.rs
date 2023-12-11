//! Logic related to parsing ostree-prepare-root.conf.
//!

// SPDX-License-Identifier: Apache-2.0 OR MIT

use anyhow::{Context, Result};
use camino::Utf8Path;
use glib::Cast;
use ostree::prelude::FileExt;
use ostree::{gio, glib};

use crate::keyfileext::KeyFileExt;
use crate::ostree_manual;

pub(crate) const CONF_PATH: &str = "ostree/prepare-root.conf";

pub(crate) fn load_config(root: &ostree::RepoFile) -> Result<Option<glib::KeyFile>> {
    let cancellable = gio::Cancellable::NONE;
    let kf = glib::KeyFile::new();
    for path in ["etc", "usr/lib"].into_iter().map(Utf8Path::new) {
        let path = &path.join(CONF_PATH);
        let f = root.resolve_relative_path(&path);
        if !f.query_exists(cancellable) {
            continue;
        }
        let f = f.downcast_ref::<ostree::RepoFile>().unwrap();
        let contents = ostree_manual::repo_file_read_to_string(f)?;
        kf.load_from_data(&contents, glib::KeyFileFlags::NONE)
            .with_context(|| format!("Parsing {path}"))?;
        tracing::debug!("Loaded {path}");
        return Ok(Some(kf));
    }
    tracing::debug!("No {CONF_PATH} found");
    Ok(None)
}

/// Query whether the target root has the `root.transient` key
/// which sets up a transient overlayfs.
pub(crate) fn overlayfs_root_enabled(root: &ostree::RepoFile) -> Result<bool> {
    if let Some(config) = load_config(root)? {
        overlayfs_enabled_in_config(&config)
    } else {
        Ok(false)
    }
}

/// Query whether the config uses an overlayfs model (composefs or plain overlayfs).
pub fn overlayfs_enabled_in_config(config: &glib::KeyFile) -> Result<bool> {
    let root_transient = config
        .optional_bool("root", "transient")?
        .unwrap_or_default();
    let required_composefs = config
        .optional_string("composefs", "enabled")?
        .map(|s| s.as_str() == "yes")
        .unwrap_or_default();
    Ok(root_transient || required_composefs)
}
