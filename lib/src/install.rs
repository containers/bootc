//! # Writing a container to a block device in a bootable way
//!
//! This module supports installing a bootc-compatible image to
//! a block device directly via the `install` verb, or to an externally
//! set up filesystem via `install-to-filesystem`.

// This sub-module is the "basic" installer that handles creating basic block device
// and filesystem setup.
mod baseline;

use std::io::BufWriter;
use std::io::Write;
use std::os::fd::AsFd;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Ok;
use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
use camino::Utf8PathBuf;
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use cap_std_ext::prelude::CapStdExtDirExt;
use chrono::prelude::*;
use clap::ValueEnum;
use ostree_ext::oci_spec;
use rustix::fs::MetadataExt;

use fn_error_context::context;
use ostree::gio;
use ostree_ext::container as ostree_container;
use ostree_ext::container::SignatureSource;
use ostree_ext::ostree;
use ostree_ext::prelude::Cast;
use serde::{Deserialize, Serialize};

use self::baseline::InstallBlockDeviceOpts;
use crate::containerenv::ContainerExecutionInfo;
use crate::hostexec::run_in_host_mountns;
use crate::task::Task;

/// The default "stateroot" or "osname"; see https://github.com/ostreedev/ostree/issues/2794
const STATEROOT_DEFAULT: &str = "default";
/// The toplevel boot directory
const BOOT: &str = "boot";
/// Directory for transient runtime state
const RUN_BOOTC: &str = "/run/bootc";
/// This is an ext4 special directory we need to ignore.
const LOST_AND_FOUND: &str = "lost+found";
pub(crate) const ARCH_USES_EFI: bool = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));

/// Kernel argument used to specify we want the rootfs mounted read-write by default
const RW_KARG: &str = "rw";

#[derive(clap::Args, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstallTargetOpts {
    // TODO: A size specifier which allocates free space for the root in *addition* to the base container image size
    // pub(crate) root_additional_size: Option<String>
    /// The transport; e.g. oci, oci-archive.  Defaults to `registry`.
    #[clap(long, default_value = "registry")]
    #[serde(default)]
    pub(crate) target_transport: String,

    /// Specify the image to fetch for subsequent updates
    #[clap(long)]
    pub(crate) target_imgref: Option<String>,

    /// Explicitly opt-out of requiring any form of signature verification.
    #[clap(long)]
    #[serde(default)]
    pub(crate) target_no_signature_verification: bool,

    /// Enable verification via an ostree remote
    #[clap(long)]
    pub(crate) target_ostree_remote: Option<String>,

    /// By default, the accessiblity of the target image will be verified (just the manifest will be fetched).
    /// Specifying this option suppresses the check; use this when you know the issues it might find
    /// are addressed.
    ///
    /// Two main reasons this might fail:
    ///
    ///  - Forgetting `--target-no-signature-verification` if needed
    ///  - Using a registry which requires authentication, but not embedding the pull secret in the image.
    #[clap(long)]
    #[serde(default)]
    pub(crate) skip_fetch_check: bool,
}

#[derive(clap::Args, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InstallConfigOpts {
    /// Disable SELinux in the target (installed) system.
    ///
    /// This is currently necessary to install *from* a system with SELinux disabled
    /// but where the target does have SELinux enabled.
    #[clap(long)]
    #[serde(default)]
    pub(crate) disable_selinux: bool,

    // Only occupy at most this much space (if no units are provided, GB is assumed).
    // Using this option reserves space for partitions created dynamically on the
    // next boot, or by subsequent tools.
    //    pub(crate) size: Option<String>,
    #[clap(long)]
    /// Add a kernel argument
    karg: Option<Vec<String>>,

    /// Perform configuration changes suitable for a "generic" disk image.
    /// At the moment:
    ///
    /// - All bootloader types will be installed
    /// - Changes to the system firmware will be skipped
    #[clap(long)]
    #[serde(default)]
    pub(crate) generic_image: bool,
}

/// Perform an installation to a block device.
#[derive(Debug, Clone, clap::Parser, Serialize, Deserialize)]
pub(crate) struct InstallOpts {
    #[clap(flatten)]
    #[serde(flatten)]
    pub(crate) block_opts: InstallBlockDeviceOpts,

    #[clap(flatten)]
    #[serde(flatten)]
    pub(crate) target_opts: InstallTargetOpts,

    #[clap(flatten)]
    #[serde(flatten)]
    pub(crate) config_opts: InstallConfigOpts,
}

#[derive(ValueEnum, Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReplaceMode {
    /// Completely wipe the contents of the target filesystem.  This cannot
    /// be done if the target filesystem is the one the system is booted from.
    Wipe,
    /// This is a destructive operation in the sense that the bootloader state
    /// will have its contents wiped and replaced.  However,
    /// the running system (and all files) will remain in place until reboot.
    ///
    /// As a corollary to this, you will also need to remove all the old operating
    /// system binaries after the reboot into the target system; this can be done
    /// with code in the new target system, or manually.
    Alongside,
}

impl std::fmt::Display for ReplaceMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value().unwrap().get_name().fmt(f)
    }
}

/// Options for installing to a filesystem
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct InstallTargetFilesystemOpts {
    /// Path to the mounted root filesystem.
    ///
    /// By default, the filesystem UUID will be discovered and used for mounting.
    /// To override this, use `--root-mount-spec`.
    pub(crate) root_path: Utf8PathBuf,

    /// Source device specification for the root filesystem.  For example, UUID=2e9f4241-229b-4202-8429-62d2302382e1
    #[clap(long)]
    pub(crate) root_mount_spec: Option<String>,

    /// Comma-separated mount options for the root filesystem.  For example: rw,prjquota
    #[clap(long)]
    pub(crate) root_options: Option<String>,

    /// Mount specification for the /boot filesystem.
    ///
    /// At the current time, a separate /boot is required.  This restriction will be lifted in
    /// future versions.  If not specified, the filesystem UUID will be used.
    #[clap(long)]
    pub(crate) boot_mount_spec: Option<String>,

    /// Initialize the system in-place; at the moment, only one mode for this is implemented.
    /// In the future, it may also be supported to set up an explicit "dual boot" system.
    #[clap(long)]
    pub(crate) replace: Option<ReplaceMode>,
}

/// Perform an installation to a mounted filesystem.
#[derive(Debug, Clone, clap::Parser)]
pub(crate) struct InstallToFilesystemOpts {
    #[clap(flatten)]
    pub(crate) filesystem_opts: InstallTargetFilesystemOpts,

    #[clap(flatten)]
    pub(crate) target_opts: InstallTargetOpts,

    #[clap(flatten)]
    pub(crate) config_opts: InstallConfigOpts,
}

/// Global state captured from the container.
#[derive(Debug, Clone)]
pub(crate) struct SourceInfo {
    /// Image reference we'll pull from (today always containers-storage: type)
    pub(crate) imageref: ostree_container::ImageReference,
    /// The digest to use for pulls
    pub(crate) digest: String,
    /// Whether or not SELinux appears to be enabled in the source commit
    pub(crate) selinux: bool,
}

// Shared read-only global state
pub(crate) struct State {
    pub(crate) source: SourceInfo,
    /// Force SELinux off in target system
    pub(crate) override_disable_selinux: bool,
    /// True if the skoepo on host supports containers-storage:
    pub(crate) skopeo_supports_containers_storage: bool,
    #[allow(dead_code)]
    pub(crate) setenforce_guard: Option<crate::lsm::SetEnforceGuard>,
    #[allow(dead_code)]
    pub(crate) config_opts: InstallConfigOpts,
    pub(crate) target_imgref: ostree_container::OstreeImageReference,
    pub(crate) install_config: config::InstallConfiguration,
}

impl State {
    // Wraps core lsm labeling functionality, conditionalizing based on source state
    pub(crate) fn lsm_label(
        &self,
        target: &Utf8Path,
        as_path: &Utf8Path,
        recurse: bool,
    ) -> Result<()> {
        if !self.source.selinux {
            return Ok(());
        }
        crate::lsm::lsm_label(target, as_path, recurse)
    }
}

/// Path to initially deployed version information
const BOOTC_ALEPH_PATH: &str = ".bootc-aleph.json";

/// The "aleph" version information is injected into /root/.bootc-aleph.json
/// and contains the image ID that was initially used to install.  This can
/// be used to trace things like the specific version of `mkfs.ext4` or
/// kernel version that was used.
#[derive(Debug, Serialize)]
struct InstallAleph {
    /// Digested pull spec for installed image
    image: String,
    /// The version number
    version: Option<String>,
    /// The timestamp
    timestamp: Option<chrono::DateTime<Utc>>,
    /// The `uname -r` of the kernel doing the installation
    kernel: String,
}

/// A mount specification is a subset of a line in `/etc/fstab`.
///
/// There are 3 (ASCII) whitespace separated values:
///
/// SOURCE TARGET [OPTIONS]
///
/// Examples:
///   - /dev/vda3 /boot ext4 ro
///   - /dev/nvme0n1p4 /
///   - /dev/sda2 /var/mnt xfs
#[derive(Debug, Clone)]
pub(crate) struct MountSpec {
    pub(crate) source: String,
    pub(crate) target: String,
    pub(crate) fstype: String,
    pub(crate) options: Option<String>,
}

impl MountSpec {
    const AUTO: &'static str = "auto";

    pub(crate) fn new(src: &str, target: &str) -> Self {
        MountSpec {
            source: src.to_string(),
            target: target.to_string(),
            fstype: Self::AUTO.to_string(),
            options: None,
        }
    }

    /// Construct a new mount that uses the provided uuid as a source.
    pub(crate) fn new_uuid_src(uuid: &str, target: &str) -> Self {
        Self::new(&format!("UUID={uuid}"), target)
    }

    pub(crate) fn get_source_uuid(&self) -> Option<&str> {
        if let Some((t, rest)) = self.source.split_once('=') {
            if t.eq_ignore_ascii_case("uuid") {
                return Some(rest);
            }
        }
        None
    }

    pub(crate) fn to_fstab(&self) -> String {
        let options = self.options.as_deref().unwrap_or("defaults");
        format!(
            "{} {} {} {} 0 0",
            self.source, self.target, self.fstype, options
        )
    }
}

impl FromStr for MountSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let mut parts = s.split_ascii_whitespace().fuse();
        let source = parts.next().unwrap_or_default();
        if source.is_empty() {
            anyhow::bail!("Invalid empty mount specification");
        }
        let target = parts
            .next()
            .ok_or_else(|| anyhow!("Missing target in mount specification {s}"))?;
        let fstype = parts.next().unwrap_or(Self::AUTO);
        let options = parts.next().map(ToOwned::to_owned);
        Ok(Self {
            source: source.to_string(),
            fstype: fstype.to_string(),
            target: target.to_string(),
            options,
        })
    }
}

impl SourceInfo {
    // Inspect container information and convert it to an ostree image reference
    // that pulls from containers-storage.
    #[context("Gathering source info from container env")]
    pub(crate) fn from_container(container_info: &ContainerExecutionInfo) -> Result<Self> {
        if !container_info.engine.starts_with("podman") {
            anyhow::bail!("Currently this command only supports being executed via podman");
        }
        if container_info.imageid.is_empty() {
            anyhow::bail!("Invalid empty imageid");
        }
        let imageref = ostree_container::ImageReference {
            transport: ostree_container::Transport::ContainerStorage,
            name: container_info.image.clone(),
        };
        let digest = crate::podman::imageid_to_digest(&container_info.imageid)?;
        let cancellable = ostree::gio::Cancellable::NONE;
        let commit = Task::new("Reading ostree commit", "ostree")
            .args(["--repo=/ostree/repo", "rev-parse", "--single"])
            .quiet()
            .read()?;
        let root = cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
        let repo = ostree::Repo::open_at_dir(root.as_fd(), "ostree/repo")?;
        let root = repo
            .read_commit(commit.trim(), cancellable)
            .context("Reading commit")?
            .0;
        let root = root.downcast_ref::<ostree::RepoFile>().unwrap();
        let xattrs = root.xattrs(cancellable)?;
        let selinux = crate::lsm::xattrs_have_selinux(&xattrs);
        Ok(Self {
            imageref,
            digest,
            selinux,
        })
    }
}

pub(crate) mod config {
    use super::*;

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
                    .get_or_insert_with(|| Default::default())
                    .extend(other_kargs)
            }
        }
    }

    #[context("Loading configuration")]
    /// Load the install configuration, merging all found configuration files.
    pub(crate) fn load_config() -> Result<InstallConfiguration> {
        const SYSTEMD_CONVENTIONAL_BASES: &[&str] = &["/usr/lib", "/usr/local/lib", "/etc", "/run"];
        let fragments =
            liboverdrop::scan(SYSTEMD_CONVENTIONAL_BASES, "bootc/install", &["toml"], true);
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
}

#[context("Creating ostree deployment")]
async fn initialize_ostree_root_from_self(
    state: &State,
    root_setup: &RootSetup,
) -> Result<InstallAleph> {
    let rootfs_dir = &root_setup.rootfs_fd;
    let rootfs = root_setup.rootfs.as_path();
    let cancellable = gio::Cancellable::NONE;

    // TODO: make configurable?
    let stateroot = STATEROOT_DEFAULT;
    Task::new_and_run(
        "Initializing ostree layout",
        "ostree",
        ["admin", "init-fs", "--modern", rootfs.as_str()],
    )?;

    // Default to avoiding grub2-mkconfig etc., but we need to use zipl on s390x.
    // TODO: Lower this logic into ostree proper.
    let bootloader = if cfg!(target_arch = "s390x") {
        "zipl"
    } else {
        "none"
    };
    for (k, v) in [
        ("sysroot.bootloader", bootloader),
        // Always flip this one on because we need to support alongside installs
        // to systems without a separate boot partition.
        ("sysroot.bootprefix", "true"),
        ("sysroot.readonly", "true"),
    ] {
        Task::new("Configuring ostree repo", "ostree")
            .args(["config", "--repo", "ostree/repo", "set", k, v])
            .cwd(rootfs_dir)?
            .quiet()
            .run()?;
    }
    Task::new("Initializing sysroot", "ostree")
        .args(["admin", "os-init", stateroot, "--sysroot", "."])
        .cwd(rootfs_dir)?
        .run()?;

    // Ensure everything in the ostree repo is labeled
    state.lsm_label(&rootfs.join("ostree"), "/usr".into(), true)?;

    let sysroot = ostree::Sysroot::new(Some(&gio::File::for_path(rootfs)));
    sysroot.load(cancellable)?;

    // We need to fetch the container image from the root mount namespace
    let skopeo_cmd = run_in_host_mountns("skopeo");
    let proxy_cfg = ostree_container::store::ImageProxyConfig {
        skopeo_cmd: Some(skopeo_cmd),
        ..Default::default()
    };

    let mut temporary_dir = None;
    let src_imageref = if state.skopeo_supports_containers_storage {
        // We always use exactly the digest of the running image to ensure predictability.
        let spec =
            crate::utils::digested_pullspec(&state.source.imageref.name, &state.source.digest);
        ostree_container::ImageReference {
            transport: ostree_container::Transport::ContainerStorage,
            name: spec,
        }
    } else {
        let td = tempfile::tempdir_in("/var/tmp")?;
        let path: &Utf8Path = td.path().try_into().unwrap();
        let r = copy_to_oci(&state.source.imageref, path)?;
        temporary_dir = Some(td);
        r
    };
    let src_imageref = ostree_container::OstreeImageReference {
        // There are no signatures to verify since we're fetching the already
        // pulled container.
        sigverify: ostree_container::SignatureSource::ContainerPolicyAllowInsecure,
        imgref: src_imageref,
    };

    let kargs = root_setup
        .kargs
        .iter()
        .map(|v| v.as_str())
        .chain(state.config_opts.karg.iter().flatten().map(|v| v.as_str()))
        .collect::<Vec<_>>();
    let mut options = ostree_container::deploy::DeployOpts::default();
    options.kargs = Some(kargs.as_slice());
    options.target_imgref = Some(&state.target_imgref);
    options.proxy_cfg = Some(proxy_cfg);
    println!("Creating initial deployment");
    let target_image = state.target_imgref.to_string();
    let state =
        ostree_container::deploy::deploy(&sysroot, stateroot, &src_imageref, Some(options)).await?;
    let digest = state.manifest_digest.as_str();
    println!("Installed: {target_image}");
    println!("   Digest: {digest}");

    drop(temporary_dir);

    // Write the entry for /boot to /etc/fstab.  TODO: Encourage OSes to use the karg?
    // Or better bind this with the grub data.
    sysroot.load(cancellable)?;
    let deployment = sysroot
        .deployments()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Failed to find deployment"))?;
    // SAFETY: There must be a path
    let path = sysroot.deployment_dirpath(&deployment);
    let root = rootfs_dir
        .open_dir(path.as_str())
        .context("Opening deployment dir")?;
    let mut f = {
        let mut opts = cap_std::fs::OpenOptions::new();
        root.open_with("etc/fstab", opts.append(true).write(true).create(true))
            .context("Opening etc/fstab")
            .map(BufWriter::new)?
    };
    if let Some(boot) = root_setup.boot.as_ref() {
        writeln!(f, "{}", boot.to_fstab())?;
    }
    f.flush()?;

    let uname = rustix::system::uname();

    let config = state.configuration.as_ref();
    let labels = config.and_then(crate::status::labels_of_config);
    let timestamp = labels
        .and_then(|l| {
            l.get(oci_spec::image::ANNOTATION_CREATED)
                .map(|s| s.as_str())
        })
        .and_then(crate::status::try_deserialize_timestamp);
    let aleph = InstallAleph {
        image: src_imageref.imgref.name.clone(),
        version: state.version().as_ref().map(|s| s.to_string()),
        timestamp,
        kernel: uname.release().to_str()?.to_string(),
    };

    Ok(aleph)
}

#[context("Copying to oci")]
fn copy_to_oci(
    src_imageref: &ostree_container::ImageReference,
    dir: &Utf8Path,
) -> Result<ostree_container::ImageReference> {
    tracing::debug!("Copying {src_imageref}");
    let src_imageref = src_imageref.to_string();
    let dest_imageref = ostree_container::ImageReference {
        transport: ostree_container::Transport::OciDir,
        name: dir.to_string(),
    };
    let dest_imageref_str = dest_imageref.to_string();
    Task::new_cmd(
        "Copying to temporary OCI (skopeo is too old)",
        run_in_host_mountns("skopeo"),
    )
    .args([
        "copy",
        // TODO: enable this once ostree is fixed "--dest-oci-accept-uncompressed-layers",
        src_imageref.as_str(),
        dest_imageref_str.as_str(),
    ])
    .run()?;
    Ok(dest_imageref)
}

#[context("Querying skopeo version")]
fn skopeo_supports_containers_storage() -> Result<bool> {
    let o = run_in_host_mountns("skopeo").arg("--version").output()?;
    let st = o.status;
    if !st.success() {
        anyhow::bail!("Failed to run skopeo --version: {st:?}");
    }
    let stdout = String::from_utf8(o.stdout).context("Parsing skopeo version")?;
    let mut v = stdout
        .strip_prefix("skopeo version ")
        .map(|v| v.split('.'))
        .ok_or_else(|| anyhow::anyhow!("Unexpected output from skopeo version"))?;
    let major = v
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing major version"))?;
    let minor = v
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing minor version"))?;
    let (major, minor) = (major.parse::<u64>()?, minor.parse::<u64>()?);
    Ok(major > 1 || minor > 10)
}

pub(crate) struct RootSetup {
    luks_device: Option<String>,
    device: Utf8PathBuf,
    rootfs: Utf8PathBuf,
    rootfs_fd: Dir,
    rootfs_uuid: Option<String>,
    /// True if this is an "alongside" install where we didn't create the filesystem
    is_alongside: bool,
    boot: Option<MountSpec>,
    kargs: Vec<String>,
}

fn require_boot_uuid(spec: &MountSpec) -> Result<&str> {
    spec.get_source_uuid()
        .ok_or_else(|| anyhow!("/boot is not specified via UUID= (this is currently required)"))
}

impl RootSetup {
    /// Get the UUID= mount specifier for the /boot filesystem; if there isn't one, the root UUID will
    /// be returned.
    fn get_boot_uuid(&self) -> Result<Option<&str>> {
        self.boot.as_ref().map(require_boot_uuid).transpose()
    }

    // Drop any open file descriptors and return just the mount path and backing luks device, if any
    fn into_storage(self) -> (Utf8PathBuf, Option<String>) {
        (self.rootfs, self.luks_device)
    }
}

/// If we detect that the target ostree commit has SELinux labels,
/// and we aren't passed an override to disable it, then ensure
/// the running process is labeled with install_t so it can
/// write arbitrary labels.
pub(crate) fn reexecute_self_for_selinux_if_needed(
    srcdata: &SourceInfo,
    override_disable_selinux: bool,
) -> Result<(bool, Option<crate::lsm::SetEnforceGuard>)> {
    let mut ret_did_override = false;
    // If the target state has SELinux enabled, we need to check the host state.
    let mut g = None;
    if srcdata.selinux {
        let host_selinux = crate::lsm::selinux_enabled()?;
        tracing::debug!("Target has SELinux, host={host_selinux}");
        if host_selinux {
            // /sys/fs/selinuxfs is not normally mounted, so we do that now.
            // Because SELinux enablement status is cached process-wide and was very likely
            // already queried by something else (e.g. glib's constructor), we would also need
            // to re-exec.  But, selinux_ensure_install does that unconditionally right now too,
            // so let's just fall through to that.
            crate::lsm::container_setup_selinux()?;
            // This will re-execute the current process (once).
            g = crate::lsm::selinux_ensure_install_or_setenforce()?;
        } else if override_disable_selinux {
            ret_did_override = true;
            println!("notice: Target has SELinux enabled, overriding to disable")
        } else {
            anyhow::bail!(
                "Host kernel does not have SELinux support, but target enables it by default"
            );
        }
    } else {
        tracing::debug!("Target does not enable SELinux");
    }
    Ok((ret_did_override, g))
}

/// Trim, flush outstanding writes, and freeze/thaw the target mounted filesystem;
/// these steps prepare the filesystem for its first booted use.
pub(crate) fn finalize_filesystem(fs: &Utf8Path) -> Result<()> {
    let fsname = fs.file_name().unwrap();
    // fstrim ensures the underlying block device knows about unused space
    Task::new_and_run(
        format!("Trimming {fsname}"),
        "fstrim",
        ["--quiet-unsupported", "-v", fs.as_str()],
    )?;
    // Remounting readonly will flush outstanding writes and ensure we error out if there were background
    // writeback problems.
    Task::new(format!("Finalizing filesystem {fsname}"), "mount")
        .args(["-o", "remount,ro", fs.as_str()])
        .run()?;
    // Finally, freezing (and thawing) the filesystem will flush the journal, which means the next boot is clean.
    for a in ["-f", "-u"] {
        Task::new("Flushing filesystem journal", "fsfreeze")
            .quiet()
            .args([a, fs.as_str()])
            .run()?;
    }
    Ok(())
}

fn require_host_pidns() -> Result<()> {
    // We require --pid=host
    if rustix::process::getpid().is_init() {
        anyhow::bail!("This command must be run with --pid=host")
    }
    Ok(())
}

// Ensure the `/var` directory exists.
fn ensure_var() -> Result<()> {
    std::fs::create_dir_all("/var")?;
    Ok(())
}

/// We want to have proper /tmp and /var/tmp without requiring the caller to set them up
/// in advance by manually specifying them via `podman run -v /tmp:/tmp` etc.
/// Unfortunately, it's quite complex right now to "gracefully" dynamically reconfigure
/// the mount setup for a container.  See https://brauner.io/2023/02/28/mounting-into-mount-namespaces.html
/// So the brutal hack we do here is to rely on the fact that we're running in the host
/// pid namespace, and so the magic link for /proc/1/root will escape our mount namespace.
/// We can't bind mount though - we need to symlink it so that each calling process
/// will traverse the link.
#[context("Linking tmp mounts to host")]
pub(crate) fn propagate_tmp_mounts_to_host() -> Result<()> {
    // Point our /tmp and /var/tmp at the host, via the /proc/1/root magic link
    for path in ["/tmp", "/var/tmp"].map(Utf8Path::new) {
        let target = format!("/proc/1/root/{path}");
        let tmp = format!("{path}.tmp");
        // Ensure idempotence in case we're re-executed
        if path.is_symlink() {
            continue;
        }
        if path.try_exists()? {
            std::os::unix::fs::symlink(&target, &tmp)
                .with_context(|| format!("Symlinking {target} to {tmp}"))?;
            let cwd = rustix::fs::CWD;
            rustix::fs::renameat_with(
                cwd,
                path.as_os_str(),
                cwd,
                &tmp,
                rustix::fs::RenameFlags::EXCHANGE,
            )
            .with_context(|| format!("Exchanging {path} <=> {tmp}"))?;
            std::fs::rename(&tmp, format!("{path}.old"))
                .with_context(|| format!("Renaming old {tmp}"))?;
        } else {
            std::os::unix::fs::symlink(&target, path)
                .with_context(|| format!("Symlinking {target} to {path}"))?;
        };
    }
    Ok(())
}

/// Verify that we can load the manifest of the target image
#[context("Verifying fetch")]
async fn verify_target_fetch(imgref: &ostree_container::OstreeImageReference) -> Result<()> {
    let tmpdir = tempfile::tempdir()?;
    let tmprepo = &ostree::Repo::new_for_path(tmpdir.path());
    tmprepo
        .create(ostree::RepoMode::Bare, ostree::gio::Cancellable::NONE)
        .context("Init tmp repo")?;

    tracing::trace!("Verifying fetch for {imgref}");
    let mut imp =
        ostree_container::store::ImageImporter::new(tmprepo, imgref, Default::default()).await?;
    use ostree_container::store::PrepareResult;
    let prep = match imp.prepare().await? {
        // SAFETY: It's impossible that the image was already fetched into this newly created temporary repository
        PrepareResult::AlreadyPresent(_) => unreachable!(),
        PrepareResult::Ready(r) => r,
    };
    tracing::debug!("Fetched manifest with digest {}", prep.manifest_digest);
    Ok(())
}

/// Preparation for an install; validates and prepares some (thereafter immutable) global state.
async fn prepare_install(
    config_opts: InstallConfigOpts,
    target_opts: InstallTargetOpts,
) -> Result<Arc<State>> {
    // We need full root privileges, i.e. --privileged in podman
    crate::cli::require_root()?;
    require_host_pidns()?;

    if cfg!(target_arch = "s390x") {
        anyhow::bail!("Installation is not supported on this architecture yet");
    }

    let rootfs = cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())
        .context("Opening /")?;

    // This command currently *must* be run inside a privileged container.
    let container_info = crate::containerenv::get_container_execution_info(&rootfs)?;
    if let Some("1") = container_info.rootless.as_deref() {
        anyhow::bail!("Cannot install from rootless podman; this command must be run as root");
    }

    let skopeo_supports_containers_storage = skopeo_supports_containers_storage()
        .context("Failed to run skopeo (it currently must be installed in the host root)")?;

    let source = SourceInfo::from_container(&container_info)?;

    // Parse the target CLI image reference options and create the *target* image
    // reference, which defaults to pulling from a registry.
    let target_sigverify = if target_opts.target_no_signature_verification {
        SignatureSource::ContainerPolicyAllowInsecure
    } else if let Some(remote) = target_opts.target_ostree_remote.as_deref() {
        SignatureSource::OstreeRemote(remote.to_string())
    } else {
        SignatureSource::ContainerPolicy
    };
    let target_imgname = target_opts
        .target_imgref
        .as_deref()
        .unwrap_or(source.imageref.name.as_str());
    let target_transport =
        ostree_container::Transport::try_from(target_opts.target_transport.as_str())?;
    let target_imgref = ostree_container::OstreeImageReference {
        sigverify: target_sigverify,
        imgref: ostree_container::ImageReference {
            transport: target_transport,
            name: target_imgname.to_string(),
        },
    };
    tracing::debug!("Target image reference: {target_imgref}");

    if !target_opts.skip_fetch_check {
        verify_target_fetch(&target_imgref).await?;
    }

    ensure_var()?;
    propagate_tmp_mounts_to_host()?;

    // Even though we require running in a container, the mounts we create should be specific
    // to this process, so let's enter a private mountns to avoid leaking them.
    if std::env::var_os("BOOTC_SKIP_UNSHARE").is_none() {
        super::cli::ensure_self_unshared_mount_namespace().await?;
    }

    // Now, deal with SELinux state.
    let (override_disable_selinux, setenforce_guard) =
        reexecute_self_for_selinux_if_needed(&source, config_opts.disable_selinux)?;

    let install_config = config::load_config()?;
    tracing::debug!("Loaded install configuration");

    // Create our global (read-only) state which gets wrapped in an Arc
    // so we can pass it to worker threads too. Right now this just
    // combines our command line options along with some bind mounts from the host.
    let state = Arc::new(State {
        override_disable_selinux,
        skopeo_supports_containers_storage,
        setenforce_guard,
        source,
        config_opts,
        target_imgref,
        install_config,
    });

    Ok(state)
}

async fn install_to_filesystem_impl(state: &State, rootfs: &mut RootSetup) -> Result<()> {
    if state.override_disable_selinux {
        rootfs.kargs.push("selinux=0".to_string());
    }

    // We verify this upfront because it's currently required by bootupd
    let boot_uuid = rootfs
        .get_boot_uuid()?
        .or(rootfs.rootfs_uuid.as_deref())
        .ok_or_else(|| anyhow!("No uuid for boot/root"))?;
    tracing::debug!("boot uuid={boot_uuid}");

    // Write the aleph data that captures the system state at the time of provisioning for aid in future debugging.
    {
        let aleph = initialize_ostree_root_from_self(state, rootfs).await?;
        rootfs
            .rootfs_fd
            .atomic_replace_with(BOOTC_ALEPH_PATH, |f| {
                serde_json::to_writer(f, &aleph)?;
                anyhow::Ok(())
            })
            .context("Writing aleph version")?;
    }

    crate::bootloader::install_via_bootupd(&rootfs.device, &rootfs.rootfs, &state.config_opts)?;
    tracing::debug!("Installed bootloader");

    // ostree likes to have the immutable bit on the physical sysroot to ensure
    // that it doesn't accumulate junk; all system state should be in deployments.
    Task::new("Setting root immutable bit", "chattr")
        .cwd(&rootfs.rootfs_fd)?
        .args(["+i", "."])
        .run()?;

    // Finalize mounted filesystems
    if !rootfs.is_alongside {
        let bootfs = rootfs.rootfs.join("boot");
        for fs in [bootfs.as_path(), rootfs.rootfs.as_path()] {
            finalize_filesystem(fs)?;
        }
    }

    Ok(())
}

fn installation_complete() {
    println!("Installation complete!");
}

/// Implementation of the `bootc install` CLI command.
pub(crate) async fn install(opts: InstallOpts) -> Result<()> {
    let block_opts = opts.block_opts;
    let state = prepare_install(opts.config_opts, opts.target_opts).await?;

    // This is all blocking stuff
    let mut rootfs = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || baseline::install_create_rootfs(&state, block_opts))
            .await??
    };

    install_to_filesystem_impl(&state, &mut rootfs).await?;

    // Drop all data about the root except the bits we need to ensure any file descriptors etc. are closed.
    let (root_path, luksdev) = rootfs.into_storage();
    Task::new_and_run(
        "Unmounting filesystems",
        "umount",
        ["-R", root_path.as_str()],
    )?;
    if let Some(luksdev) = luksdev.as_deref() {
        Task::new_and_run("Closing root LUKS device", "cryptsetup", ["close", luksdev])?;
    }

    installation_complete();

    Ok(())
}

#[context("Verifying empty rootfs")]
fn require_empty_rootdir(rootfs_fd: &Dir) -> Result<()> {
    for e in rootfs_fd.entries()? {
        let e = e?;
        let name = e.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("Invalid non-UTF8 filename: {name:?}"))?;
        if name == LOST_AND_FOUND {
            continue;
        }
        // There must be a boot directory (that is empty)
        if name == BOOT {
            let mut entries = rootfs_fd.read_dir(BOOT)?;
            if let Some(e) = entries.next() {
                let e = e?;
                let name = e.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| anyhow!("Invalid non-UTF8 filename: {name:?}"))?;
                if matches!(name, LOST_AND_FOUND | crate::bootloader::EFI_DIR) {
                    continue;
                }
                anyhow::bail!("Non-empty boot directory, found {name:?}");
            }
        } else {
            anyhow::bail!("Non-empty root filesystem; found {name:?}");
        }
    }
    Ok(())
}

/// Remove all entries in a directory, but do not traverse across distinct devices.
#[context("Removing entries (noxdev")]
fn remove_all_in_dir_no_xdev(d: &Dir) -> Result<()> {
    let parent_dev = d.dir_metadata()?.dev();
    for entry in d.entries()? {
        let entry = entry?;
        let entry_dev = entry.metadata()?.dev();
        if entry_dev == parent_dev {
            d.remove_all_optional(entry.file_name())?;
        }
    }
    anyhow::Ok(())
}

#[context("Removing boot directory content")]
fn clean_boot_directories(rootfs: &Dir) -> Result<()> {
    let bootdir = rootfs.open_dir(BOOT).context("Opening /boot")?;
    // This should not remove /boot/efi note.
    remove_all_in_dir_no_xdev(&bootdir)?;
    if ARCH_USES_EFI {
        if let Some(efidir) = bootdir
            .open_dir_optional(crate::bootloader::EFI_DIR)
            .context("Opening /boot/efi")?
        {
            remove_all_in_dir_no_xdev(&efidir)?;
        }
    }
    Ok(())
}

/// Implementation of the `bootc install-to-filsystem` CLI command.
pub(crate) async fn install_to_filesystem(opts: InstallToFilesystemOpts) -> Result<()> {
    // Gather global state, destructuring the provided options
    let state = prepare_install(opts.config_opts, opts.target_opts).await?;
    let fsopts = opts.filesystem_opts;

    let root_path = &fsopts.root_path;
    let rootfs_fd = Dir::open_ambient_dir(root_path, cap_std::ambient_authority())
        .with_context(|| format!("Opening target root directory {root_path}"))?;
    match fsopts.replace {
        Some(ReplaceMode::Wipe) => {
            let rootfs_fd = rootfs_fd.try_clone()?;
            println!("Wiping contents of root");
            tokio::task::spawn_blocking(move || {
                for e in rootfs_fd.entries()? {
                    let e = e?;
                    rootfs_fd.remove_all_optional(e.file_name())?;
                }
                anyhow::Ok(())
            })
            .await??;
        }
        Some(ReplaceMode::Alongside) => clean_boot_directories(&rootfs_fd)?,
        None => require_empty_rootdir(&rootfs_fd)?,
    }

    // Gather data about the root filesystem
    let inspect = crate::mount::inspect_filesystem(&fsopts.root_path)?;

    // We support overriding the mount specification for root (i.e. LABEL vs UUID versus
    // raw paths).
    let (root_mount_spec, root_extra) = if let Some(s) = fsopts.root_mount_spec {
        (s, None)
    } else {
        let uuid = inspect
            .uuid
            .as_deref()
            .ok_or_else(|| anyhow!("No filesystem uuid found in target root"))?;
        let uuid = format!("UUID={uuid}");
        tracing::debug!("root {uuid}");
        let opts = match inspect.fstype.as_str() {
            "btrfs" => {
                let subvol = crate::utils::find_mount_option(&inspect.options, "subvol");
                subvol.map(|vol| format!("rootflags=subvol={vol}"))
            }
            _ => None,
        };
        (uuid, opts)
    };
    tracing::debug!("Root mount spec: {root_mount_spec}");

    let boot_is_mount = {
        let root_dev = rootfs_fd.dir_metadata()?.dev();
        let boot_dev = rootfs_fd
            .symlink_metadata_optional(BOOT)?
            .ok_or_else(|| {
                anyhow!("No /{BOOT} directory found in root; this is is currently required")
            })?
            .dev();
        tracing::debug!("root_dev={root_dev} boot_dev={boot_dev}");
        root_dev != boot_dev
    };
    // Find the UUID of /boot because we need it for GRUB.
    let boot_uuid = if boot_is_mount {
        let boot_path = fsopts.root_path.join(BOOT);
        let u = crate::mount::inspect_filesystem(&boot_path)
            .context("Inspecting /{BOOT}")?
            .uuid
            .ok_or_else(|| anyhow!("No UUID found for /{BOOT}"))?;
        Some(u)
    } else {
        None
    };
    tracing::debug!("boot UUID: {boot_uuid:?}");

    // Find the real underlying backing device for the root.  This is currently just required
    // for GRUB (BIOS) and in the future zipl (I think).
    let backing_device = {
        let mut dev = inspect.source;
        loop {
            tracing::debug!("Finding parents for {dev}");
            let mut parents = crate::blockdev::find_parent_devices(&dev)?.into_iter();
            let parent = if let Some(f) = parents.next() {
                f
            } else {
                break;
            };
            if let Some(next) = parents.next() {
                anyhow::bail!(
                    "Found multiple parent devices {parent} and {next}; not currently supported"
                );
            }
            dev = parent;
        }
        dev
    };
    tracing::debug!("Backing device: {backing_device}");

    let rootarg = format!("root={root_mount_spec}");
    let boot = if let Some(spec) = fsopts.boot_mount_spec {
        Some(MountSpec::new(&spec, "/boot"))
    } else {
        boot_uuid
            .as_deref()
            .map(|boot_uuid| MountSpec::new_uuid_src(boot_uuid, "/boot"))
    };
    // By default, we inject a boot= karg because things like FIPS compliance currently
    // require checking in the initramfs.
    let bootarg = boot.as_ref().map(|boot| format!("boot={}", &boot.source));
    let kargs = [rootarg]
        .into_iter()
        .chain(root_extra)
        .chain([RW_KARG.to_string()])
        .chain(bootarg)
        .collect::<Vec<_>>();

    let mut rootfs = RootSetup {
        luks_device: None,
        device: backing_device.into(),
        rootfs: fsopts.root_path,
        rootfs_fd,
        rootfs_uuid: inspect.uuid.clone(),
        boot,
        kargs,
        is_alongside: matches!(fsopts.replace, Some(ReplaceMode::Alongside)),
    };

    install_to_filesystem_impl(&state, &mut rootfs).await?;

    // Drop all data about the root except the path to ensure any file descriptors etc. are closed.
    drop(rootfs);

    installation_complete();

    Ok(())
}

#[test]
fn install_opts_serializable() {
    let c: InstallOpts = serde_json::from_value(serde_json::json!({
        "device": "/dev/vda"
    }))
    .unwrap();
    assert_eq!(c.block_opts.device, "/dev/vda");
}
