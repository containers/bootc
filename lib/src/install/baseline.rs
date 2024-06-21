//! # The baseline installer
//!
//! This module handles creation of simple root filesystem setups.  At the current time
//! it's very simple - just a direct filesystem (e.g. xfs, ext4, btrfs etc.).  It is
//! intended to add opinionated handling of TPM2-bound LUKS too.  But that's about it;
//! other more complex flows should set things up externally and use `bootc install to-filesystem`.

use std::borrow::Cow;
use std::fmt::Display;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;

use anyhow::Ok;
use anyhow::{Context, Result};
use camino::Utf8Path;
use camino::Utf8PathBuf;
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use clap::ValueEnum;
use fn_error_context::context;
use serde::{Deserialize, Serialize};

use super::MountSpec;
use super::RootSetup;
use super::State;
use super::RUN_BOOTC;
use super::RW_KARG;
use crate::mount;
use crate::task::Task;

pub(crate) const BOOTPN: u32 = 3;
// This ensures we end up under 512 to be small-sized.
pub(crate) const BOOTPN_SIZE_MB: u32 = 510;
pub(crate) const EFIPN: u32 = 2;
pub(crate) const EFIPN_SIZE_MB: u32 = 512;

#[derive(clap::ValueEnum, Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Filesystem {
    Xfs,
    Ext4,
    Btrfs,
}

impl Display for Filesystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value().unwrap().get_name().fmt(f)
    }
}

#[derive(clap::ValueEnum, Default, Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BlockSetup {
    #[default]
    Direct,
    Tpm2Luks,
}

impl Display for BlockSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value().unwrap().get_name().fmt(f)
    }
}

/// Options for installing to a block device
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct InstallBlockDeviceOpts {
    /// Target block device for installation.  The entire device will be wiped.
    pub(crate) device: Utf8PathBuf,

    /// Automatically wipe all existing data on device
    #[clap(long)]
    #[serde(default)]
    pub(crate) wipe: bool,

    /// Target root block device setup.
    ///
    /// direct: Filesystem written directly to block device
    /// tpm2-luks: Bind unlock of filesystem to presence of the default tpm2 device.
    #[clap(long, value_enum)]
    pub(crate) block_setup: Option<BlockSetup>,

    /// Target root filesystem type.
    #[clap(long, value_enum)]
    pub(crate) filesystem: Option<Filesystem>,

    /// Size of the root partition (default specifier: M).  Allowed specifiers: M (mebibytes), G (gibibytes), T (tebibytes).
    ///
    /// By default, all remaining space on the disk will be used.
    #[clap(long)]
    pub(crate) root_size: Option<String>,
}

impl BlockSetup {
    /// Returns true if the block setup requires a separate /boot aka XBOOTLDR partition.
    pub(crate) fn requires_bootpart(&self) -> bool {
        match self {
            BlockSetup::Direct => false,
            BlockSetup::Tpm2Luks => true,
        }
    }
}

fn sgdisk_partition(
    sgdisk: &mut Command,
    n: u32,
    part: impl AsRef<str>,
    name: impl AsRef<str>,
    typecode: Option<&str>,
) {
    sgdisk.arg("-n");
    sgdisk.arg(format!("{n}:{}", part.as_ref()));
    sgdisk.arg("-c");
    sgdisk.arg(format!("{n}:{}", name.as_ref()));
    if let Some(typecode) = typecode {
        sgdisk.arg("-t");
        sgdisk.arg(format!("{n}:{typecode}"));
    }
}

fn mkfs<'a>(
    dev: &str,
    fs: Filesystem,
    label: &str,
    opts: impl IntoIterator<Item = &'a str>,
) -> Result<uuid::Uuid> {
    let devinfo = crate::blockdev::list_dev(dev.into())?;
    let size = devinfo.size.as_deref().unwrap_or("(unknown)");
    let u = uuid::Uuid::new_v4();
    let mut t = Task::new(
        &format!("Creating {label} filesystem ({fs}) on device {dev} (size={size})"),
        format!("mkfs.{fs}"),
    );
    match fs {
        Filesystem::Xfs => {
            t.cmd.arg("-m");
            t.cmd.arg(format!("uuid={u}"));
        }
        Filesystem::Btrfs | Filesystem::Ext4 => {
            t.cmd.arg("-U");
            t.cmd.arg(u.to_string());
        }
    };
    // Today all the above mkfs commands take -L
    t.cmd.args(["-L", label]);
    t.cmd.args(opts);
    t.cmd.arg(dev);
    // All the mkfs commands are unnecessarily noisy by default
    t.cmd.stdout(Stdio::null());
    // But this one is notable so let's print the whole thing with verbose()
    t.verbose().run()?;
    Ok(u)
}

#[context("Creating rootfs")]
pub(crate) fn install_create_rootfs(
    state: &State,
    opts: InstallBlockDeviceOpts,
) -> Result<RootSetup> {
    let luks_name = "root";
    // Ensure we have a root filesystem upfront
    let root_filesystem = opts
        .filesystem
        .or(state
            .install_config
            .as_ref()
            .and_then(|c| c.filesystem_root())
            .and_then(|r| r.fstype))
        .ok_or_else(|| anyhow::anyhow!("No root filesystem specified"))?;
    // Verify that the target is empty (if not already wiped in particular, but it's
    // also good to verify that the wipe worked)
    let device = crate::blockdev::list_dev(&opts.device)?;
    // Canonicalize devpath
    let devpath: Utf8PathBuf = device.path().into();

    // Handle wiping any existing data
    if opts.wipe {
        let dev = &opts.device;
        for child in device.children.iter().flatten() {
            let child = child.path();
            println!("Wiping {child}");
            crate::blockdev::wipefs(Utf8Path::new(&child))?;
        }
        println!("Wiping {dev}");
        crate::blockdev::wipefs(dev)?;
    } else if device.has_children() {
        anyhow::bail!(
            "Detected existing partitions on {}; use e.g. `wipefs` if you intend to overwrite",
            opts.device
        );
    }

    let run_bootc = Utf8Path::new(RUN_BOOTC);
    let mntdir = run_bootc.join("mounts");
    if mntdir.exists() {
        std::fs::remove_dir_all(&mntdir)?;
    }

    // Use the install configuration to find the block setup, if we have one
    let block_setup = if let Some(config) = state.install_config.as_ref() {
        config.get_block_setup(opts.block_setup.as_ref().copied())?
    } else if opts.filesystem.is_some() {
        // Otherwise, if a filesystem is specified then we default to whatever was
        // specified via --block-setup, or the default
        opts.block_setup.unwrap_or_default()
    } else {
        // If there was no default filesystem, then there's no default block setup,
        // and we need to error out.
        anyhow::bail!("No install configuration found, and no filesystem specified")
    };
    let serial = device.serial.as_deref().unwrap_or("<unknown>");
    let model = device.model.as_deref().unwrap_or("<unknown>");
    let size = device
        .size
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Missing size for blockdev"))?;
    println!("Block setup: {block_setup}");
    println!("       Size: {size}",);
    println!("     Serial: {serial}");
    println!("      Model: {model}");

    let root_size = opts
        .root_size
        .as_deref()
        .map(crate::blockdev::parse_size_mib)
        .transpose()
        .context("Parsing root size")?;

    // Load the policy from the container root, which also must be our install root
    let sepolicy = state.load_policy()?;
    let sepolicy = sepolicy.as_ref();

    // Create a temporary directory to use for mount points.  Note that we're
    // in a mount namespace, so these should not be visible on the host.
    let rootfs = mntdir.join("rootfs");
    std::fs::create_dir_all(&rootfs)?;
    let bootfs = mntdir.join("boot");
    std::fs::create_dir_all(bootfs)?;

    // Run sgdisk to create partitions.
    let mut sgdisk = Task::new("Initializing partitions", "sgdisk");
    // sgdisk is too verbose
    sgdisk.cmd.stdout(Stdio::null());
    sgdisk.cmd.arg("-Z");
    sgdisk.cmd.arg(device.path());
    sgdisk.cmd.args(["-U", "R"]);
    #[allow(unused_assignments)]
    if cfg!(target_arch = "x86_64") {
        // BIOS-BOOT
        sgdisk_partition(
            &mut sgdisk.cmd,
            1,
            "0:+1M",
            "BIOS-BOOT",
            Some("21686148-6449-6E6F-744E-656564454649"),
        );
    } else if cfg!(target_arch = "aarch64") {
        // reserved
        sgdisk_partition(
            &mut sgdisk.cmd,
            1,
            "0:+1M",
            "reserved",
            Some("8DA63339-0007-60C0-C436-083AC8230908"),
        );
    } else {
        anyhow::bail!("Unsupported architecture: {}", std::env::consts::ARCH);
    }

    let esp_partno = if super::ARCH_USES_EFI {
        sgdisk_partition(
            &mut sgdisk.cmd,
            EFIPN,
            format!("0:+{EFIPN_SIZE_MB}M"),
            "EFI-SYSTEM",
            Some("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"),
        );
        Some(EFIPN)
    } else {
        None
    };

    // Initialize the /boot filesystem.  Note that in the future, we may match
    // what systemd/uapi-group encourages and make /boot be FAT32 as well, as
    // it would aid systemd-boot.
    let use_xbootldr = block_setup.requires_bootpart();
    let mut partno = EFIPN;
    if use_xbootldr {
        partno += 1;
        sgdisk_partition(
            &mut sgdisk.cmd,
            partno,
            format!("0:+{BOOTPN_SIZE_MB}M"),
            "boot",
            None,
        );
    }
    let rootpn = if use_xbootldr { BOOTPN + 1 } else { EFIPN + 1 };
    let root_size = root_size
        .map(|v| Cow::Owned(format!("0:{v}M")))
        .unwrap_or_else(|| Cow::Borrowed("0:0"));
    sgdisk_partition(
        &mut sgdisk.cmd,
        rootpn,
        root_size,
        "root",
        Some("0FC63DAF-8483-4772-8E79-3D69D8477DE4"),
    );
    sgdisk.run().context("Failed to run sgdisk")?;
    tracing::debug!("Created partition table");

    // Reread the partition table
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&devpath)
            .with_context(|| format!("opening {devpath}"))?;
        crate::blockdev::reread_partition_table(&mut f, true)
            .context("Rereading partition table")?;
    }

    // Full udev sync; it'd obviously be better to await just the devices
    // we're targeting, but this is a simple coarse hammer.
    crate::blockdev::udev_settle()?;

    // Now inspect the partitioned device again so we can find the names of the child devices.
    let device_partitions = crate::blockdev::list_dev(&devpath)?
        .children
        .ok_or_else(|| anyhow::anyhow!("Failed to find children after partitioning"))?;
    // Given a partition number, return the path to its device.
    let findpart = |idx: u32| -> Result<String> {
        // checked_sub is here because our partition numbers start at 1, but the vec starts at 0
        let devpath = device_partitions
            .get(idx.checked_sub(1).unwrap() as usize)
            .ok_or_else(|| anyhow::anyhow!("Missing partition for index {idx}"))?
            .path();
        Ok(devpath)
    };

    let base_rootdev = findpart(rootpn)?;

    let (rootdev, root_blockdev_kargs) = match block_setup {
        BlockSetup::Direct => (base_rootdev, None),
        BlockSetup::Tpm2Luks => {
            let uuid = uuid::Uuid::new_v4().to_string();
            // This will be replaced via --wipe-slot=all when binding to tpm below
            let dummy_passphrase = uuid::Uuid::new_v4().to_string();
            let mut tmp_keyfile = tempfile::NamedTempFile::new()?;
            tmp_keyfile.write_all(dummy_passphrase.as_bytes())?;
            tmp_keyfile.flush()?;
            let tmp_keyfile = tmp_keyfile.path();
            let dummy_passphrase_input = Some(dummy_passphrase.as_bytes());

            Task::new("Initializing LUKS for root", "cryptsetup")
                .args(["luksFormat", "--uuid", uuid.as_str(), "--key-file"])
                .args([tmp_keyfile])
                .args([base_rootdev.as_str()])
                .run()?;
            // The --wipe-slot=all removes our temporary passphrase, and binds to the local TPM device.
            // We also use .verbose() here as the details are important/notable.
            Task::new("Enrolling root device with TPM", "systemd-cryptenroll")
                .args(["--wipe-slot=all", "--tpm2-device=auto", "--unlock-key-file"])
                .args([tmp_keyfile])
                .args([base_rootdev.as_str()])
                .verbose()
                .run_with_stdin_buf(dummy_passphrase_input)?;
            Task::new("Opening root LUKS device", "cryptsetup")
                .args(["luksOpen", base_rootdev.as_str(), luks_name])
                .run()?;
            let rootdev = format!("/dev/mapper/{luks_name}");
            let kargs = vec![
                format!("luks.uuid={uuid}"),
                format!("luks.options=tpm2-device=auto,headless=true"),
            ];
            (rootdev, Some(kargs))
        }
    };

    // Initialize the /boot filesystem
    let bootdev = if use_xbootldr {
        Some(findpart(BOOTPN)?)
    } else {
        None
    };
    let boot_uuid = if let Some(bootdev) = bootdev.as_deref() {
        Some(mkfs(bootdev, root_filesystem, "boot", []).context("Initializing /boot")?)
    } else {
        None
    };

    // Initialize rootfs
    let root_uuid = mkfs(&rootdev, root_filesystem, "root", [])?;
    let rootarg = format!("root=UUID={root_uuid}");
    let bootsrc = boot_uuid.as_ref().map(|uuid| format!("UUID={uuid}"));
    let bootarg = bootsrc.as_deref().map(|bootsrc| format!("boot={bootsrc}"));
    let boot = bootsrc.map(|bootsrc| MountSpec {
        source: bootsrc,
        target: "/boot".into(),
        fstype: MountSpec::AUTO.into(),
        options: Some("ro".into()),
    });
    let kargs = root_blockdev_kargs
        .into_iter()
        .flatten()
        .chain([rootarg, RW_KARG.to_string()].into_iter())
        .chain(bootarg)
        .collect::<Vec<_>>();

    mount::mount(&rootdev, &rootfs)?;
    let target_rootfs = Dir::open_ambient_dir(&rootfs, cap_std::ambient_authority())?;
    crate::lsm::ensure_dir_labeled(&target_rootfs, "", Some("/".into()), 0o755.into(), sepolicy)?;
    let rootfs_fd = Dir::open_ambient_dir(&rootfs, cap_std::ambient_authority())?;
    let bootfs = rootfs.join("boot");
    // Create the underlying mount point directory, which should be labeled
    crate::lsm::ensure_dir_labeled(&target_rootfs, "boot", None, 0o755.into(), sepolicy)?;
    if let Some(bootdev) = bootdev.as_deref() {
        mount::mount(bootdev, &bootfs)?;
    }
    // And we want to label the root mount of /boot
    crate::lsm::ensure_dir_labeled(&target_rootfs, "boot", None, 0o755.into(), sepolicy)?;

    // Create the EFI system partition, if applicable
    if let Some(esp_partno) = esp_partno {
        let espdev = &findpart(esp_partno)?;
        Task::new("Creating ESP filesystem", "mkfs.fat")
            .args([espdev.as_str(), "-n", "EFI-SYSTEM"])
            .verbose()
            .quiet_output()
            .run()?;
        let efifs_path = bootfs.join(crate::bootloader::EFI_DIR);
        std::fs::create_dir(&efifs_path).context("Creating efi dir")?;
        mount::mount(espdev, &efifs_path)?;
    }

    let luks_device = match block_setup {
        BlockSetup::Direct => None,
        BlockSetup::Tpm2Luks => Some(luks_name.to_string()),
    };
    Ok(RootSetup {
        luks_device,
        device: devpath,
        rootfs,
        rootfs_fd,
        rootfs_uuid: Some(root_uuid.to_string()),
        boot,
        kargs,
        skip_finalize: false,
    })
}
