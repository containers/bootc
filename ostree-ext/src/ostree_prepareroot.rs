//! Logic related to parsing ostree-prepare-root.conf.
//!

// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::str::FromStr;

use anyhow::{Context, Result};
use camino::Utf8Path;
use glib::Cast;
use ostree::prelude::FileExt;
use ostree::{gio, glib};

use crate::keyfileext::KeyFileExt;
use crate::ostree_manual;
use crate::utils::ResultExt;

pub(crate) const CONF_PATH: &str = "ostree/prepare-root.conf";

pub(crate) fn load_config(root: &ostree::RepoFile) -> Result<Option<glib::KeyFile>> {
    let cancellable = gio::Cancellable::NONE;
    let kf = glib::KeyFile::new();
    for path in ["etc", "usr/lib"].into_iter().map(Utf8Path::new) {
        let path = &path.join(CONF_PATH);
        let f = root.resolve_relative_path(path);
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

#[derive(Debug, PartialEq, Eq)]
enum Tristate {
    Enabled,
    Disabled,
    Maybe,
}

impl FromStr for Tristate {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let r = match s {
            // Keep this in sync with ot_keyfile_get_tristate_with_default from ostree
            "yes" | "true" | "1" => Tristate::Enabled,
            "no" | "false" | "0" => Tristate::Disabled,
            "maybe" => Tristate::Maybe,
            o => anyhow::bail!("Invalid tristate value: {o}"),
        };
        Ok(r)
    }
}

impl Default for Tristate {
    fn default() -> Self {
        Self::Disabled
    }
}

impl Tristate {
    pub(crate) fn maybe_enabled(&self) -> bool {
        match self {
            Self::Enabled | Self::Maybe => true,
            Self::Disabled => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ComposefsState {
    Signed,
    Tristate(Tristate),
}

impl Default for ComposefsState {
    fn default() -> Self {
        Self::Tristate(Tristate::default())
    }
}

impl FromStr for ComposefsState {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let r = match s {
            "signed" => Self::Signed,
            o => Self::Tristate(Tristate::from_str(o)?),
        };
        Ok(r)
    }
}

impl ComposefsState {
    pub(crate) fn maybe_enabled(&self) -> bool {
        match self {
            ComposefsState::Signed => true,
            ComposefsState::Tristate(t) => t.maybe_enabled(),
        }
    }
}

/// Query whether the config uses an overlayfs model (composefs or plain overlayfs).
pub fn overlayfs_enabled_in_config(config: &glib::KeyFile) -> Result<bool> {
    let root_transient = config
        .optional_bool("root", "transient")?
        .unwrap_or_default();
    let composefs = config
        .optional_string("composefs", "enabled")?
        .map(|s| ComposefsState::from_str(s.as_str()))
        .transpose()
        .log_err_default()
        .unwrap_or_default();
    Ok(root_transient || composefs.maybe_enabled())
}

#[test]
fn test_tristate() {
    for v in ["yes", "true", "1"] {
        assert_eq!(Tristate::from_str(v).unwrap(), Tristate::Enabled);
    }
    assert_eq!(Tristate::from_str("maybe").unwrap(), Tristate::Maybe);
    for v in ["no", "false", "0"] {
        assert_eq!(Tristate::from_str(v).unwrap(), Tristate::Disabled);
    }
    for v in ["", "junk", "fal", "tr1"] {
        assert!(Tristate::from_str(v).is_err());
    }
}

#[test]
fn test_composefs_state() {
    assert_eq!(
        ComposefsState::from_str("signed").unwrap(),
        ComposefsState::Signed
    );
    for v in ["yes", "true", "1"] {
        assert_eq!(
            ComposefsState::from_str(v).unwrap(),
            ComposefsState::Tristate(Tristate::Enabled)
        );
    }
    assert_eq!(Tristate::from_str("maybe").unwrap(), Tristate::Maybe);
    for v in ["no", "false", "0"] {
        assert_eq!(
            ComposefsState::from_str(v).unwrap(),
            ComposefsState::Tristate(Tristate::Disabled)
        );
    }
}

#[test]
fn test_overlayfs_enabled() {
    let d0 = indoc::indoc! { r#"
[foo]
bar = baz
[root]
"# };
    let d1 = indoc::indoc! { r#"
[root]
transient = false
    "# };
    let d2 = indoc::indoc! { r#"
[composefs]
enabled = false
    "# };
    for v in ["", d0, d1, d2] {
        let kf = glib::KeyFile::new();
        kf.load_from_data(v, glib::KeyFileFlags::empty()).unwrap();
        assert_eq!(overlayfs_enabled_in_config(&kf).unwrap(), false);
    }

    let e0 = format!("{d0}\n[root]\ntransient = true");
    let e1 = format!("{d1}\n[composefs]\nenabled = true\n[other]\nsomekey = someval");
    let e2 = format!("{d1}\n[composefs]\nenabled = yes");
    let e3 = format!("{d1}\n[composefs]\nenabled = signed");
    for v in [e0, e1, e2, e3] {
        let kf = glib::KeyFile::new();
        kf.load_from_data(&v, glib::KeyFileFlags::empty()).unwrap();
        assert_eq!(overlayfs_enabled_in_config(&kf).unwrap(), true);
    }
}
