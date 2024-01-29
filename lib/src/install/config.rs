//! # Configuration for `bootc install`
//!
//! This module handles the TOML configuration file for `bootc install`.

use anyhow::{Context, Result};
use fn_error_context::context;
use serde::{Deserialize, Serialize};

/// The toplevel config entry for installation configs stored
/// in bootc/install (e.g. /etc/bootc/install/05-custom.toml)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallConfigurationToplevel {
    pub(crate) install: Option<InstallConfiguration>,
}

/// Configuration for a filesystem
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootFS {
    #[serde(rename = "type")]
    pub(crate) fstype: Option<super::baseline::Filesystem>,
}

/// This structure should only define "system" or "basic" filesystems; we are
/// not trying to generalize this into e.g. supporting `/var` or other ones.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct BasicFilesystems {
    pub(crate) root: Option<RootFS>,
    // TODO allow configuration of these other filesystems too
    // pub(crate) xbootldr: Option<FilesystemCustomization>,
    // pub(crate) esp: Option<FilesystemCustomization>,
}

/// The serialized [install] section
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename = "install", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct InstallConfiguration {
    /// Root filesystem type
    pub(crate) root_fs_type: Option<super::baseline::Filesystem>,
    pub(crate) filesystem: Option<BasicFilesystems>,
    /// Kernel arguments, applied at installation time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kargs: Option<Vec<String>>,
}

fn merge_basic<T>(s: &mut Option<T>, o: Option<T>) {
    if let Some(o) = o {
        *s = Some(o);
    }
}

trait Mergeable {
    fn merge(&mut self, other: Self)
    where
        Self: Sized;
}

impl<T> Mergeable for Option<T>
where
    T: Mergeable,
{
    fn merge(&mut self, other: Self)
    where
        Self: Sized,
    {
        if let Some(other) = other {
            if let Some(s) = self.as_mut() {
                s.merge(other)
            } else {
                *self = Some(other);
            }
        }
    }
}

impl Mergeable for RootFS {
    /// Apply any values in other, overriding any existing values in `self`.
    fn merge(&mut self, other: Self) {
        merge_basic(&mut self.fstype, other.fstype)
    }
}

impl Mergeable for BasicFilesystems {
    /// Apply any values in other, overriding any existing values in `self`.
    fn merge(&mut self, other: Self) {
        self.root.merge(other.root)
    }
}

impl Mergeable for InstallConfiguration {
    /// Apply any values in other, overriding any existing values in `self`.
    fn merge(&mut self, other: Self) {
        merge_basic(&mut self.root_fs_type, other.root_fs_type);
        self.filesystem.merge(other.filesystem);
        if let Some(other_kargs) = other.kargs {
            self.kargs
                .get_or_insert_with(Default::default)
                .extend(other_kargs)
        }
    }
}

impl InstallConfiguration {
    /// Some fields can be specified multiple ways.  This synchronizes the values of the fields
    /// to ensure they're the same.
    ///
    /// - install.root-fs-type is synchronized with install.filesystems.root.type; if
    ///   both are set, then the latter takes precedence
    pub(crate) fn canonicalize(&mut self) {
        // New canonical form wins.
        if let Some(rootfs_type) = self.filesystem_root().and_then(|f| f.fstype.as_ref()) {
            self.root_fs_type = Some(*rootfs_type)
        } else if let Some(rootfs) = self.root_fs_type.as_ref() {
            let fs = self.filesystem.get_or_insert_with(Default::default);
            let root = fs.root.get_or_insert_with(Default::default);
            root.fstype = Some(*rootfs);
        }
    }

    /// Convenience helper to access the root filesystem
    pub(crate) fn filesystem_root(&self) -> Option<&RootFS> {
        self.filesystem.as_ref().and_then(|fs| fs.root.as_ref())
    }

    // Remove all configuration which is handled by `install to-filesystem`.
    pub(crate) fn filter_to_external(&mut self) {
        self.kargs.take();
    }
}

#[context("Loading configuration")]
/// Load the install configuration, merging all found configuration files.
pub(crate) fn load_config() -> Result<InstallConfiguration> {
    const SYSTEMD_CONVENTIONAL_BASES: &[&str] = &["/usr/lib", "/usr/local/lib", "/etc", "/run"];
    let fragments = liboverdrop::scan(SYSTEMD_CONVENTIONAL_BASES, "bootc/install", &["toml"], true);
    let mut config: Option<InstallConfiguration> = None;
    for (_name, path) in fragments {
        let buf = std::fs::read_to_string(&path)?;
        let mut unused = std::collections::HashSet::new();
        let de = toml::Deserializer::new(&buf);
        let c: InstallConfigurationToplevel = serde_ignored::deserialize(de, |path| {
            unused.insert(path.to_string());
        })
        .with_context(|| format!("Parsing {path:?}"))?;
        for key in unused {
            eprintln!("warning: {path:?}: Unknown key {key}");
        }
        if let Some(config) = config.as_mut() {
            if let Some(install) = c.install {
                tracing::debug!("Merging install config: {install:?}");
                config.merge(install);
            }
        } else {
            config = c.install;
        }
    }
    let mut config = config.ok_or_else(|| anyhow::anyhow!("No bootc/install config found; this operating system must define a default configuration to be installable"))?;
    config.canonicalize();
    Ok(config)
}

#[test]
/// Verify that we can parse our default config file
fn test_parse_config() {
    use super::baseline::Filesystem;

    let c: InstallConfigurationToplevel = toml::from_str(
        r##"[install]
root-fs-type = "xfs"
"##,
    )
    .unwrap();
    let mut install = c.install.unwrap();
    assert_eq!(install.root_fs_type.unwrap(), Filesystem::Xfs);
    let other = InstallConfigurationToplevel {
        install: Some(InstallConfiguration {
            root_fs_type: Some(Filesystem::Ext4),
            filesystem: None,
            kargs: None,
        }),
    };
    install.merge(other.install.unwrap());
    assert_eq!(
        install.root_fs_type.as_ref().copied().unwrap(),
        Filesystem::Ext4
    );
    // This one shouldn't have been set
    assert!(install.filesystem_root().is_none());
    install.canonicalize();
    assert_eq!(install.root_fs_type.as_ref().unwrap(), &Filesystem::Ext4);
    assert_eq!(
        install.filesystem_root().unwrap().fstype.unwrap(),
        Filesystem::Ext4
    );

    let c: InstallConfigurationToplevel = toml::from_str(
        r##"[install]
root-fs-type = "ext4"
kargs = ["console=ttyS0", "foo=bar"]
"##,
    )
    .unwrap();
    let mut install = c.install.unwrap();
    assert_eq!(install.root_fs_type.unwrap(), Filesystem::Ext4);
    let other = InstallConfigurationToplevel {
        install: Some(InstallConfiguration {
            root_fs_type: None,
            filesystem: None,
            kargs: Some(
                ["console=tty0", "nosmt"]
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .collect(),
            ),
        }),
    };
    install.merge(other.install.unwrap());
    assert_eq!(install.root_fs_type.unwrap(), Filesystem::Ext4);
    assert_eq!(
        install.kargs,
        Some(
            ["console=ttyS0", "foo=bar", "console=tty0", "nosmt"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect()
        )
    )
}

#[test]
fn test_parse_filesystems() {
    use super::baseline::Filesystem;
    let c: InstallConfigurationToplevel = toml::from_str(
        r##"[install.filesystem.root]
type = "xfs"
"##,
    )
    .unwrap();
    let mut install = c.install.unwrap();
    assert_eq!(
        install.filesystem_root().unwrap().fstype.unwrap(),
        Filesystem::Xfs
    );
    let other = InstallConfigurationToplevel {
        install: Some(InstallConfiguration {
            root_fs_type: None,
            filesystem: Some(BasicFilesystems {
                root: Some(RootFS {
                    fstype: Some(Filesystem::Ext4),
                }),
            }),
            kargs: None,
        }),
    };
    install.merge(other.install.unwrap());
    assert_eq!(
        install.filesystem_root().unwrap().fstype.unwrap(),
        Filesystem::Ext4
    );
}
