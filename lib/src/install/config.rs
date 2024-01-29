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

/// The serialized [install] section
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename = "install", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct InstallConfiguration {
    /// Root filesystem type
    pub(crate) root_fs_type: Option<super::baseline::Filesystem>,
    /// Kernel arguments, applied at installation time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kargs: Option<Vec<String>>,
}

impl InstallConfiguration {
    /// Apply any values in other, overriding any existing values in `self`.
    fn merge(&mut self, other: Self) {
        fn mergeopt<T>(s: &mut Option<T>, o: Option<T>) {
            if let Some(o) = o {
                *s = Some(o);
            }
        }
        mergeopt(&mut self.root_fs_type, other.root_fs_type);
        if let Some(other_kargs) = other.kargs {
            self.kargs
                .get_or_insert_with(Default::default)
                .extend(other_kargs)
        }
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
    config.ok_or_else(|| anyhow::anyhow!("No bootc/install config found; this operating system must define a default configuration to be installable"))
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
            kargs: None,
        }),
    };
    install.merge(other.install.unwrap());
    assert_eq!(install.root_fs_type.unwrap(), Filesystem::Ext4);

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
