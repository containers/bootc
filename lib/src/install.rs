use std::borrow::Cow;
use std::fmt::Display;
use std::io::BufWriter;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result};
use camino::Utf8Path;
use camino::Utf8PathBuf;
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use cap_std_ext::prelude::CapStdExtDirExt;
use clap::ArgEnum;
use fn_error_context::context;
use ostree::gio;
use ostree_ext::container as ostree_container;
use ostree_ext::container::SignatureSource;
use ostree_ext::ostree;
use ostree_ext::prelude::Cast;
use serde::Serialize;

use crate::containerenv::ContainerExecutionInfo;
use crate::lsm::lsm_label;
use crate::task::Task;
use crate::utils::run_in_host_mountns;

/// The default "stateroot" or "osname"; see https://github.com/ostreedev/ostree/issues/2794
const STATEROOT_DEFAULT: &str = "default";

/// Directory for transient runtime state
const RUN_BOOTC: &str = "/run/bootc";

#[derive(clap::ValueEnum, Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum BlockSetup {
    Direct,
    Tpm2Luks,
}

impl Default for BlockSetup {
    fn default() -> Self {
        Self::Direct
    }
}

#[derive(clap::ValueEnum, Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Filesystem {
    Xfs,
    Ext4,
    Btrfs,
}

impl Default for Filesystem {
    fn default() -> Self {
        // Obviously this should be configurable.
        Self::Xfs
    }
}

impl Display for Filesystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_possible_value().unwrap().get_name().fmt(f)
    }
}

/// Kernel argument used to specify we want the rootfs mounted read-write by default
const RW_KARG: &str = "rw";

const BOOTPN: u32 = 3;
// This ensures we end up under 512 to be small-sized.
const BOOTPN_SIZE_MB: u32 = 510;
const ROOTPN: u32 = 4;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const EFIPN: u32 = 2;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const EFIPN_SIZE_MB: u32 = 512;
#[cfg(target_arch = "aarch64")]
const RESERVEDPN: u32 = 1;
#[cfg(target_arch = "ppc64")]
const PREPPN: u32 = 1;
#[cfg(target_arch = "ppc64")]
const RESERVEDPN: u32 = 1;

/// Perform an upgrade operation
#[derive(Debug, Clone, clap::Parser)]
pub(crate) struct InstallOpts {
    /// Target block device for installation.  The entire device will be wiped.
    pub(crate) device: Utf8PathBuf,

    /// Automatically wipe all existing data on device
    #[clap(long)]
    pub(crate) wipe: bool,

    /// Size of the root partition (default specifier: M).  Allowed specifiers: M (mebibytes), G (gibibytes), T (tebibytes).
    ///
    /// By default, all remaining space on the disk will be used.
    #[clap(long)]
    pub(crate) root_size: Option<String>,

    // TODO: A size specifier which allocates free space for the root in *addition* to the base container image size
    // pub(crate) root_additional_size: Option<String>
    /// The transport; e.g. oci, oci-archive.  Defaults to `registry`.
    #[clap(long, default_value = "registry")]
    pub(crate) target_transport: String,

    /// Specify the image to fetch for subsequent updates
    #[clap(long)]
    pub(crate) target_imgref: Option<String>,

    /// Explicitly opt-out of requiring any form of signature verification.
    #[clap(long)]
    pub(crate) target_no_signature_verification: bool,

    /// Enable verification via an ostree remote
    #[clap(long)]
    pub(crate) target_ostree_remote: Option<String>,

    /// Target root filesystem type.
    #[clap(long, value_enum, default_value_t)]
    pub(crate) filesystem: Filesystem,

    /// Path to an Ignition config file
    #[clap(long, value_parser)]
    pub(crate) ignition_file: Option<Utf8PathBuf>,

    /// Digest (type-value) of the Ignition config
    ///
    /// Verify that the Ignition config matches the specified digest,
    /// formatted as <type>-<hexvalue>.  <type> can be sha256 or sha512.
    #[clap(long, value_name = "digest", value_parser)]
    pub(crate) ignition_hash: Option<crate::ignition::IgnitionHash>,

    /// Target root block device setup.
    ///
    /// direct: Filesystem written directly to block device
    /// tpm2-luks: Bind unlock of filesystem to presence of the default tpm2 device.
    #[clap(long, value_enum, default_value_t)]
    pub(crate) block_setup: BlockSetup,

    /// Disable SELinux in the target (installed) system.
    ///
    /// This is currently necessary to install *from* a system with SELinux disabled
    /// but where the target does have SELinux enabled.
    #[clap(long)]
    pub(crate) disable_selinux: bool,

    // Only occupy at most this much space (if no units are provided, GB is assumed).
    // Using this option reserves space for partitions created dynamically on the
    // next boot, or by subsequent tools.
    //    pub(crate) size: Option<String>,
    #[clap(long)]
    /// Add a kernel argument
    karg: Option<Vec<String>>,
}

// Shared read-only global state
struct State {
    opts: InstallOpts,
    /// Path to our devtmpfs
    devdir: Utf8PathBuf,
    mntdir: Utf8PathBuf,
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
    kernel: String,
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
    label: Option<&'_ str>,
    opts: impl IntoIterator<Item = &'a str>,
) -> Result<uuid::Uuid> {
    let u = uuid::Uuid::new_v4();
    let mut t = Task::new("Creating filesystem", &format!("mkfs.{fs}"));
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
    if let Some(label) = label {
        t.cmd.args(["-L", label]);
    }
    t.cmd.args(opts);
    t.cmd.arg(dev);
    // All the mkfs commands are unnecessarily noisy by default
    t.cmd.stdout(Stdio::null());
    t.run()?;
    Ok(u)
}

fn mount(dev: &str, target: &Utf8Path) -> Result<()> {
    Task::new_and_run(
        &format!("Mounting {target}"),
        "mount",
        [dev, target.as_str()],
    )
}

fn bind_mount_from_host(src: impl AsRef<Utf8Path>, dest: impl AsRef<Utf8Path>) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();
    tracing::debug!("Mounting host {src} to {dest}");
    std::fs::create_dir_all(dest).with_context(|| format!("Creating {dest}"))?;
    // Here's the magic trick; modern versions of the `mount` command support a `-N` argument
    // to perform the mount in a distinct target namespace.  But, what we want to is the inverse
    // of this - we want to grab a host/root filesystem mount point.  So we explicitly enter
    // the host's mount namespace, then give `mount` our own pid (from which it finds the mount namespace).
    let desc = format!("Bind mounting {src} from host");
    let target = format!("{}", nix::unistd::getpid());
    Task::new_cmd(&desc, run_in_host_mountns("mount"))
        .quiet()
        .args(["--bind", "-N", target.as_str(), src.as_str(), dest.as_str()])
        .run()
}

#[context("Creating ostree deployment")]
async fn initialize_ostree_root_from_self(
    state: &State,
    containerstate: &ContainerExecutionInfo,
    root_setup: &RootSetup,
    kargs: &[&str],
) -> Result<InstallAleph> {
    let rootfs = root_setup.rootfs.as_path();
    let opts = &state.opts;
    let cancellable = gio::Cancellable::NONE;

    if !containerstate.engine.starts_with("podman") {
        anyhow::bail!("Currently this command only supports being executed via podman");
    }
    if containerstate.imageid.is_empty() {
        anyhow::bail!("Invalid empty imageid");
    }
    let digest = crate::podman::imageid_to_digest(&containerstate.imageid)?;
    let src_image = crate::utils::digested_pullspec(&containerstate.image, &digest);

    let src_imageref = ostree_container::OstreeImageReference {
        sigverify: ostree_container::SignatureSource::ContainerPolicyAllowInsecure,
        imgref: ostree_container::ImageReference {
            transport: ostree_container::Transport::ContainerStorage,
            name: src_image.clone(),
        },
    };

    // Parse the target CLI image reference options
    let target_sigverify = if opts.target_no_signature_verification {
        SignatureSource::ContainerPolicyAllowInsecure
    } else if let Some(remote) = opts.target_ostree_remote.as_deref() {
        SignatureSource::OstreeRemote(remote.to_string())
    } else {
        SignatureSource::ContainerPolicy
    };
    let target_imgref = if let Some(imgref) = opts.target_imgref.as_ref() {
        let transport = ostree_container::Transport::try_from(opts.target_transport.as_str())?;
        let imgref = ostree_container::ImageReference {
            transport,
            name: imgref.to_string(),
        };
        ostree_container::OstreeImageReference {
            sigverify: target_sigverify,
            imgref,
        }
    } else {
        ostree_container::OstreeImageReference {
            sigverify: target_sigverify,
            imgref: ostree_container::ImageReference {
                transport: ostree_container::Transport::Registry,
                name: containerstate.image.clone(),
            },
        }
    };

    // TODO: make configurable?
    let stateroot = STATEROOT_DEFAULT;
    Task::new_and_run(
        "Initializing ostree layout",
        "ostree",
        ["admin", "init-fs", "--modern", rootfs.as_str()],
    )?;

    let repopath = &rootfs.join("ostree/repo");
    for (k, v) in [("sysroot.bootloader", "none"), ("sysroot.readonly", "true")] {
        Task::new("Configuring ostree repo", "ostree")
            .args(["config", "--repo", repopath.as_str(), "set", k, v])
            .quiet()
            .run()?;
    }
    Task::new_and_run(
        "Initializing sysroot",
        "ostree",
        ["admin", "os-init", stateroot, "--sysroot", rootfs.as_str()],
    )?;

    // Ensure everything in the ostree repo is labeled
    lsm_label(&rootfs.join("ostree"), "/usr".into(), true)?;

    let sysroot = ostree::Sysroot::new(Some(&gio::File::for_path(rootfs)));
    sysroot.load(cancellable)?;

    // We need to fetch the container image from the root mount namespace
    let skopeo_cmd = run_in_host_mountns("skopeo");
    let proxy_cfg = ostree_container::store::ImageProxyConfig {
        skopeo_cmd: Some(skopeo_cmd),
        ..Default::default()
    };

    let mut temporary_dir = None;
    let src_imageref = if skopeo_supports_containers_storage()? {
        src_imageref
    } else {
        let td = tempfile::tempdir_in("/var/tmp")?;
        let path: &Utf8Path = td.path().try_into().unwrap();
        let r = copy_to_oci(&src_imageref, path)?;
        temporary_dir = Some(td);
        r
    };

    #[allow(clippy::needless_update)]
    let options = ostree_container::deploy::DeployOpts {
        kargs: Some(kargs),
        target_imgref: Some(&target_imgref),
        proxy_cfg: Some(proxy_cfg),
        ..Default::default()
    };
    println!("Creating initial deployment");
    let state =
        ostree_container::deploy::deploy(&sysroot, stateroot, &src_imageref, Some(options)).await?;
    let target_image = target_imgref.to_string();
    let digest = state.manifest_digest;
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
    let path = sysroot.deployment_dirpath(&deployment).unwrap();
    let sysroot_dir = cap_std::fs::Dir::open_ambient_dir(rootfs, cap_std::ambient_authority())
        .context("Opening rootfs")?;
    let root = sysroot_dir
        .open_dir(path.as_str())
        .context("Opening deployment dir")?;
    let mut f = {
        let mut opts = cap_std::fs::OpenOptions::new();
        root.open_with("etc/fstab", opts.append(true).write(true).create(true))
            .context("Opening etc/fstab")
            .map(BufWriter::new)?
    };
    let boot_uuid = &root_setup.boot_uuid;
    let bootfs_type_str = root_setup.bootfs_type.to_string();
    writeln!(f, "UUID={boot_uuid} /boot {bootfs_type_str} defaults 1 2")?;
    f.flush()?;

    let uname = cap_std_ext::rustix::process::uname();

    let aleph = InstallAleph {
        image: src_image,
        kernel: uname.release().to_str()?.to_string(),
    };

    Ok(aleph)
}

#[context("Copying to oci")]
fn copy_to_oci(
    src_imageref: &ostree_container::OstreeImageReference,
    dir: &Utf8Path,
) -> Result<ostree_container::OstreeImageReference> {
    tracing::debug!("Copying {src_imageref}");
    let src_imageref = &src_imageref.imgref.to_string();
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
    Ok(ostree_container::OstreeImageReference {
        sigverify: SignatureSource::ContainerPolicyAllowInsecure,
        imgref: dest_imageref,
    })
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

struct RootSetup {
    device: Utf8PathBuf,
    rootfs: Utf8PathBuf,
    rootfs_fd: Dir,
    bootfs_type: Filesystem,
    boot_uuid: uuid::Uuid,
    kargs: Vec<String>,
}

#[context("Creating rootfs")]
fn install_create_rootfs(state: &State) -> Result<RootSetup> {
    let opts = &state.opts;
    // Verify that the target is empty (if not already wiped in particular, but it's
    // also good to verify that the wipe worked)
    let device = crate::blockdev::list_dev(&opts.device)?;

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

    // Now at this point, our /dev is a stale snapshot because we don't have udev running.
    // So from hereon after, we prefix devices with our temporary devtmpfs mount.
    let reldevice = opts
        .device
        .strip_prefix("/dev/")
        .context("Absolute device path in /dev/ required")?;
    let device = state.devdir.join(reldevice);

    let root_size = opts
        .root_size
        .as_deref()
        .map(crate::blockdev::parse_size_mib)
        .transpose()
        .context("Parsing root size")?;

    // Create a temporary directory to use for mount points.  Note that we're
    // in a mount namespace, so these should not be visible on the host.
    let rootfs = state.mntdir.join("rootfs");
    std::fs::create_dir_all(&rootfs)?;
    let bootfs = state.mntdir.join("boot");
    std::fs::create_dir_all(&bootfs)?;

    // Run sgdisk to create partitions.
    let mut sgdisk = Task::new("Initializing partitions", "sgdisk");
    // sgdisk is too verbose
    sgdisk.cmd.stdout(Stdio::null());
    sgdisk.cmd.arg("-Z");
    sgdisk.cmd.arg(&device);
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

    let espdev = if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        sgdisk_partition(
            &mut sgdisk.cmd,
            EFIPN,
            format!("0:+{EFIPN_SIZE_MB}M"),
            "EFI-SYSTEM",
            Some("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"),
        );
        Some(format!("{device}{EFIPN}"))
    } else {
        None
    };

    sgdisk_partition(
        &mut sgdisk.cmd,
        BOOTPN,
        format!("0:+{BOOTPN_SIZE_MB}M"),
        "boot",
        None,
    );
    let root_size = root_size
        .map(|v| Cow::Owned(format!("0:{v}M")))
        .unwrap_or_else(|| Cow::Borrowed("0:0"));
    sgdisk_partition(
        &mut sgdisk.cmd,
        ROOTPN,
        root_size,
        "root",
        Some("0FC63DAF-8483-4772-8E79-3D69D8477DE4"),
    );
    sgdisk.run()?;

    // Reread the partition table
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&device)
            .with_context(|| format!("opening {device}"))?;
        crate::blockdev::reread_partition_table(&mut f, true)
            .context("Rereading partition table")?;
    }

    crate::blockdev::udev_settle()?;

    match opts.block_setup {
        BlockSetup::Direct => {}
        // TODO
        BlockSetup::Tpm2Luks => anyhow::bail!("tpm2-luks is not implemented yet"),
    }

    // TODO: make this configurable
    let bootfs_type = Filesystem::Ext4;

    // Initialize the /boot filesystem
    let bootdev = &format!("{device}{BOOTPN}");
    let boot_uuid = mkfs(bootdev, bootfs_type, Some("boot"), []).context("Initializing /boot")?;

    // Initialize rootfs
    let rootdev = &format!("{device}{ROOTPN}");
    let root_uuid = mkfs(rootdev, opts.filesystem, Some("root"), [])?;
    let rootarg = format!("root=UUID={root_uuid}");
    let bootarg = format!("boot=UUID={boot_uuid}");
    let kargs = vec![rootarg, RW_KARG.to_string(), bootarg];

    mount(rootdev, &rootfs)?;
    lsm_label(&rootfs, "/".into(), false)?;
    let rootfs_fd = Dir::open_ambient_dir(&rootfs, cap_std::ambient_authority())?;
    let bootfs = rootfs.join("boot");
    std::fs::create_dir(&bootfs).context("Creating /boot")?;
    // The underlying directory on the root should be labeled
    lsm_label(&bootfs, "/boot".into(), false)?;
    mount(bootdev, &bootfs)?;
    // And we want to label the root mount of /boot
    lsm_label(&bootfs, "/boot".into(), false)?;

    // Create the EFI system partition, if applicable
    if let Some(espdev) = espdev {
        Task::new("Creating ESP filesystem", "mkfs.fat")
            .args([espdev.as_str(), "-n", "EFI-SYSTEM"])
            .quiet_output()
            .run()?;
        let efifs_path = bootfs.join("efi");
        std::fs::create_dir(&efifs_path).context("Creating efi dir")?;
        mount(&espdev, &efifs_path)?;
    }

    Ok(RootSetup {
        device,
        rootfs,
        rootfs_fd,
        bootfs_type,
        boot_uuid,
        kargs,
    })
}

struct SourceData {
    /// The embedded base OSTree commit checksum
    #[allow(dead_code)]
    commit: String,
    /// Whether or not SELinux appears to be enabled in the source commit
    selinux: bool,
}

#[context("Gathering source data")]
fn gather_source_data() -> Result<SourceData> {
    let cancellable = ostree::gio::Cancellable::NONE;
    let commit = Task::new("Reading ostree commit", "ostree")
        .args(["--repo=/ostree/repo", "rev-parse", "--single"])
        .quiet()
        .read()?;
    let root = cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    let repo = ostree::Repo::open_at_dir(&root, "ostree/repo")?;
    let root = repo
        .read_commit(commit.trim(), cancellable)
        .context("Reading commit")?
        .0;
    let root = root.downcast_ref::<ostree::RepoFile>().unwrap();
    let xattrs = root.xattrs(cancellable)?;
    let selinux = crate::lsm::xattrs_have_selinux(&xattrs);
    Ok(SourceData { commit, selinux })
}

/// Implementation of the `bootc install` CLI command.
pub(crate) async fn install(opts: InstallOpts) -> Result<()> {
    // This command currently *must* be run inside a privileged container.
    let container_state = crate::containerenv::get_container_execution_info()?;

    // We require --pid=host
    let pid = std::fs::read_link("/proc/1/exe").context("reading /proc/1/exe")?;
    let pid = pid
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Non-UTF8 /proc/1/exe"))?;
    if !pid.contains("systemd") {
        anyhow::bail!("This command must be run with --pid=host")
    }

    // Even though we require running in a container, the mounts we create should be specific
    // to this process, so let's enter a private mountns to avoid leaking them.
    if std::env::var_os("BOOTC_SKIP_UNSHARE").is_none() {
        super::cli::ensure_self_unshared_mount_namespace().await?;
    }

    // Let's ensure we have a tmpfs on /tmp, because we need that to write the SELinux label
    // (it won't work on the default overlayfs)
    if nix::sys::statfs::statfs("/tmp")?.filesystem_type() != nix::sys::statfs::TMPFS_MAGIC {
        Task::new("Creating tmpfs on /tmp", "mount")
            .quiet()
            .args(["-t", "tmpfs", "tmpfs", "/tmp"])
            .run()?;
    }

    // Now, deal with SELinux state.
    let srcdata = gather_source_data()?;
    let mut override_disable_selinux = false;
    // If the target state has SELinux enabled, we need to check the host state.
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
            crate::lsm::selinux_ensure_install()?;
        } else if opts.disable_selinux {
            override_disable_selinux = true;
            println!("notice: Target has SELinux enabled, overriding to disable")
        } else {
            anyhow::bail!(
                "Host kernel does not have SELinux support, but target enables it by default"
            );
        }
    } else {
        tracing::debug!("Target does not enable SELinux");
    }

    // Create our global (read-only) state which gets wrapped in an Arc
    // so we can pass it to worker threads too. Right now this just
    // combines our command line options along with some bind mounts from the host.
    let run_bootc = Utf8Path::new(RUN_BOOTC);
    let mntdir = run_bootc.join("mounts");
    if mntdir.exists() {
        std::fs::remove_dir_all(&mntdir)?;
    }
    let devdir = mntdir.join("dev");
    std::fs::create_dir_all(&devdir)?;
    Task::new_and_run(
        "Mounting devtmpfs",
        "mount",
        ["devtmpfs", "-t", "devtmpfs", devdir.as_str()],
    )?;
    // Overmount /var/tmp with the host's, so we can use it to share state
    bind_mount_from_host("/var/tmp", "/var/tmp")?;
    let state = Arc::new(State {
        mntdir,
        devdir,
        opts,
    });

    // This is all blocking stuff
    let rootfs = {
        let state = state.clone();
        tokio::task::spawn_blocking(move || install_create_rootfs(&state)).await??
    };
    let mut kargs = rootfs.kargs.iter().map(|v| v.as_str()).collect::<Vec<_>>();
    if override_disable_selinux {
        kargs.push("selinux=0");
    }
    // This is interpreted by our GRUB fragment
    if state.opts.ignition_file.is_some() {
        kargs.push(crate::ignition::PLATFORM_METAL_KARG);
        kargs.push(crate::bootloader::IGNITION_VARIABLE);
    }

    // Write the aleph data that captures the system state at the time of provisioning for aid in future debugging.
    {
        let aleph =
            initialize_ostree_root_from_self(&state, &container_state, &rootfs, kargs.as_slice())
                .await?;
        rootfs
            .rootfs_fd
            .atomic_replace_with(BOOTC_ALEPH_PATH, |f| {
                serde_json::to_writer(f, &aleph)?;
                anyhow::Ok(())
            })
            .context("Writing aleph version")?;
    }

    crate::bootloader::install_via_bootupd(&rootfs.device, &rootfs.rootfs, &rootfs.boot_uuid)?;

    // If Ignition is specified, enable it
    if let Some(ignition_file) = state.opts.ignition_file.as_deref() {
        let src = std::fs::File::open(ignition_file)
            .with_context(|| format!("Opening {ignition_file}"))?;
        let bootfs = rootfs.rootfs.join("boot");
        crate::ignition::write_ignition(&bootfs, &state.opts.ignition_hash, &src)?;
        crate::ignition::enable_firstboot(&bootfs)?;
        println!("Installed Ignition config from {ignition_file}");
    }

    Task::new_and_run(
        "Setting root immutable bit",
        "chattr",
        ["+i", rootfs.rootfs.as_str()],
    )?;

    Task::new_and_run("Trimming filesystems", "fstrim", ["-a", "-v"])?;

    let bootfs = rootfs.rootfs.join("boot");
    for fs in [bootfs.as_path(), rootfs.rootfs.as_path()] {
        let fsname = fs.file_name().unwrap();
        Task::new(&format!("Finalizing filesystem {fsname}"), "mount")
            .args(["-o", "remount,ro", fs.as_str()])
            .run()?;
        for a in ["-f", "-u"] {
            Task::new("Flushing filesystem journal", "xfs_freeze")
                .quiet()
                .args([a, fs.as_str()])
                .run()?;
        }
    }

    // Drop all data about the root except the path to ensure any file descriptors etc. are closed.
    let rootfs_path = rootfs.rootfs.clone();
    drop(rootfs);

    Task::new_and_run(
        "Unmounting filesystems",
        "umount",
        ["-R", rootfs_path.as_str()],
    )?;

    println!("Installation complete!");

    Ok(())
}
