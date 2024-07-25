//! # Bootable container image CLI
//!
//! Command line tool to manage bootable ostree-based containers.

use std::ffi::OsString;
use std::io::Seek;
use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use cap_std_ext::cap_std;
use cap_std_ext::cap_std::fs::Dir;
use clap::Parser;
use clap::ValueEnum;
use fn_error_context::context;
use ostree::gio;
use ostree_container::store::PrepareResult;
use ostree_ext::container as ostree_container;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;
use schemars::schema_for;

use crate::deploy::RequiredHostSpec;
use crate::lints;
use crate::spec::Host;
use crate::spec::ImageReference;
use crate::utils::sigpolicy_from_opts;

include!(concat!(env!("OUT_DIR"), "/version.rs"));

/// Perform an upgrade operation
#[derive(Debug, Parser, PartialEq, Eq)]
pub(crate) struct UpgradeOpts {
    /// Don't display progress
    #[clap(long)]
    pub(crate) quiet: bool,

    /// Check if an update is available without applying it.
    ///
    /// This only downloads an updated manifest and image configuration (i.e. typically kilobyte-sized metadata)
    /// as opposed to the image layers.
    #[clap(long, conflicts_with = "apply")]
    pub(crate) check: bool,

    /// Restart or reboot into the new target image.
    ///
    /// Currently, this option always reboots.  In the future this command
    /// will detect the case where no kernel changes are queued, and perform
    /// a userspace-only restart.
    #[clap(long, conflicts_with = "check")]
    pub(crate) apply: bool,
}

/// Perform an switch operation
#[derive(Debug, Parser, PartialEq, Eq)]
pub(crate) struct SwitchOpts {
    /// Don't display progress
    #[clap(long)]
    pub(crate) quiet: bool,

    /// The transport; e.g. oci, oci-archive, containers-storage.  Defaults to `registry`.
    #[clap(long, default_value = "registry")]
    pub(crate) transport: String,

    /// This argument is deprecated and does nothing.
    #[clap(long, hide = true)]
    pub(crate) no_signature_verification: bool,

    /// This is the inverse of the previous `--target-no-signature-verification` (which is now
    /// a no-op).
    ///
    /// Enabling this option enforces that `/etc/containers/policy.json` includes a
    /// default policy which requires signatures.
    #[clap(long)]
    pub(crate) enforce_container_sigpolicy: bool,

    /// Enable verification via an ostree remote
    #[clap(long)]
    pub(crate) ostree_remote: Option<String>,

    /// Don't create a new deployment, but directly mutate the booted state.
    /// This is hidden because it's not something we generally expect to be done,
    /// but this can be used in e.g. Anaconda %post to fixup
    #[clap(long, hide = true)]
    pub(crate) mutate_in_place: bool,

    /// Retain reference to currently booted image
    #[clap(long)]
    pub(crate) retain: bool,

    /// Target image to use for the next boot.
    pub(crate) target: String,
}

/// Options controlling rollback
#[derive(Debug, Parser, PartialEq, Eq)]
pub(crate) struct RollbackOpts {}

/// Perform an edit operation
#[derive(Debug, Parser, PartialEq, Eq)]
pub(crate) struct EditOpts {
    /// Use filename to edit system specification
    #[clap(long, short = 'f')]
    pub(crate) filename: Option<String>,

    /// Don't display progress
    #[clap(long)]
    pub(crate) quiet: bool,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
#[clap(rename_all = "lowercase")]
pub(crate) enum OutputFormat {
    /// Output in YAML format.
    Yaml,
    /// Output in JSON format.
    Json,
}

/// Perform an status operation
#[derive(Debug, Parser, PartialEq, Eq)]
pub(crate) struct StatusOpts {
    /// Output in JSON format.
    ///
    /// Superceded by the `format` option.
    #[clap(long, hide = true)]
    pub(crate) json: bool,

    /// The output format.
    #[clap(long)]
    pub(crate) format: Option<OutputFormat>,

    /// The desired format version. There is currently one supported
    /// version, which is version `0`. Pass this option to explicitly
    /// request it; it is possible that multiple versions will be
    /// supported in the future.
    #[clap(long)]
    pub(crate) format_version: Option<u32>,

    /// Only display status for the booted deployment.
    #[clap(long)]
    pub(crate) booted: bool,
}

#[cfg(feature = "install")]
#[derive(Debug, clap::Subcommand, PartialEq, Eq)]
pub(crate) enum InstallOpts {
    /// Install to the target block device.
    ///
    /// This command must be invoked inside of the container, which will be
    /// installed. The container must be run in `--privileged` mode, and hence
    /// will be able to see all block devices on the system.
    ///
    /// The default storage layout uses the root filesystem type configured
    /// in the container image, alongside any required system partitions such as
    /// the EFI system partition. Use `install to-filesystem` for anything more
    /// complex such as RAID, LVM, LUKS etc.
    ToDisk(crate::install::InstallToDiskOpts),
    /// Install to an externally created filesystem structure.
    ///
    /// In this variant of installation, the root filesystem alongside any necessary
    /// platform partitions (such as the EFI system partition) are prepared and mounted by an
    /// external tool or script. The root filesystem is currently expected to be empty
    /// by default.
    ToFilesystem(crate::install::InstallToFilesystemOpts),
    /// Install to the host root filesystem.
    ///
    /// This is a variant of `install to-filesystem` that is designed to install "alongside"
    /// the running host root filesystem. Currently, the host root filesystem's `/boot` partition
    /// will be wiped, but the content of the existing root will otherwise be retained, and will
    /// need to be cleaned up if desired when rebooted into the new root.
    ToExistingRoot(crate::install::InstallToExistingRootOpts),
    /// Output JSON to stdout that contains the merged installation configuration
    /// as it may be relevant to calling processes using `install to-filesystem`
    /// that in particular want to discover the desired root filesystem type from the container image.
    ///
    /// At the current time, the only output key is `root-fs-type` which is a string-valued
    /// filesystem name suitable for passing to `mkfs.$type`.
    PrintConfiguration,
}

/// Options for man page generation
#[derive(Debug, Parser, PartialEq, Eq)]
pub(crate) struct ManOpts {
    #[clap(long)]
    /// Output to this directory
    pub(crate) directory: Utf8PathBuf,
}

/// Subcommands which can be executed as part of a container build.
#[derive(Debug, clap::Subcommand, PartialEq, Eq)]
pub(crate) enum ContainerOpts {
    /// Perform relatively inexpensive static analysis checks as part of a container
    /// build.
    ///
    /// This is intended to be invoked via e.g. `RUN bootc container lint` as part
    /// of a build process; it will error if any problems are detected.
    Lint,
}

/// Subcommands which operate on images.
#[derive(Debug, clap::Subcommand, PartialEq, Eq)]
pub(crate) enum ImageOpts {
    /// List fetched images stored in the bootc storage.
    ///
    /// Note that these are distinct from images stored via e.g. `podman`.
    List,
    /// Copy a container image from the bootc storage to `containers-storage:`.
    ///
    /// The source and target are both optional; if both are left unspecified,
    /// via a simple invocation of `bootc image copy-to-storage`, then the default is to
    /// push the currently booted image to `containers-storage` (as used by podman, etc.)
    /// and tagged with the image name `localhost/bootc`,
    ///
    /// ## Copying a non-default container image
    ///
    /// It is also possible to copy an image other than the currently booted one by
    /// specifying `--source`.
    ///
    /// ## Pulling images
    ///
    /// At the current time there is no explicit support for pulling images other than indirectly
    /// via e.g. `bootc switch` or `bootc upgrade`.
    CopyToStorage {
        #[clap(long)]
        /// The source image; if not specified, the booted image will be used.
        source: Option<String>,

        #[clap(long)]
        /// The destination; if not specified, then the default is to push to `containers-storage:localhost/bootc`;
        /// this will make the image accessible via e.g. `podman run localhost/bootc` and for builds.
        target: Option<String>,
    },
}

/// Hidden, internal only options
#[derive(Debug, clap::Subcommand, PartialEq, Eq)]
pub(crate) enum InternalsOpts {
    SystemdGenerator {
        normal_dir: Utf8PathBuf,
        #[allow(dead_code)]
        early_dir: Option<Utf8PathBuf>,
        #[allow(dead_code)]
        late_dir: Option<Utf8PathBuf>,
    },
    FixupEtcFstab,
    /// Should only be used by `make update-generated`
    PrintJsonSchema,
}

impl InternalsOpts {
    /// The name of the binary we inject into /usr/lib/systemd/system-generators
    const GENERATOR_BIN: &'static str = "bootc-systemd-generator";
}

/// Deploy and transactionally in-place with bootable container images.
///
/// The `bootc` project currently uses ostree-containers as a backend
/// to support a model of bootable container images.  Once installed,
/// whether directly via `bootc install` (executed as part of a container)
/// or via another mechanism such as an OS installer tool, further
/// updates can be pulled and `bootc upgrade`.
#[derive(Debug, Parser, PartialEq, Eq)]
#[clap(name = "bootc")]
#[clap(rename_all = "kebab-case")]
#[clap(version,long_version=CLAP_LONG_VERSION)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Opt {
    /// Download and queue an updated container image to apply.
    ///
    /// This does not affect the running system; updates operate in an "A/B" style by default.
    ///
    /// A queued update is visible as `staged` in `bootc status`.
    ///
    /// Currently by default, the update will be applied at shutdown time via `ostree-finalize-staged.service`.
    /// There is also an explicit `bootc upgrade --apply` verb which will automatically take action (rebooting)
    /// if the system has changed.
    ///
    /// However, in the future this is likely to change such that reboots outside of a `bootc upgrade --apply`
    /// do *not* automatically apply the update in addition.
    #[clap(alias = "update")]
    Upgrade(UpgradeOpts),
    /// Target a new container image reference to boot.
    ///
    /// This is almost exactly the same operation as `upgrade`, but additionally changes the container image reference
    /// instead.
    ///
    /// ## Usage
    ///
    /// A common pattern is to have a management agent control operating system updates via container image tags;
    /// for example, `quay.io/exampleos/someuser:v1.0` and `quay.io/exampleos/someuser:v1.1` where some machines
    /// are tracking `:v1.0`, and as a rollout progresses, machines can be switched to `v:1.1`.
    Switch(SwitchOpts),
    /// Change the bootloader entry ordering; the deployment under `rollback` will be queued for the next boot,
    /// and the current will become rollback.  If there is a `staged` entry (an unapplied, queued upgrade)
    /// then it will be discarded.
    ///
    /// Note that absent any additional control logic, if there is an active agent doing automated upgrades
    /// (such as the default `bootc-fetch-apply-updates.timer` and associated `.service`) the
    /// change here may be reverted.  It's recommended to only use this in concert with an agent that
    /// is in active control.
    ///
    /// A systemd journal message will be logged with `MESSAGE_ID=26f3b1eb24464d12aa5e7b544a6b5468` in
    /// order to detect a rollback invocation.
    Rollback(RollbackOpts),
    /// Apply full changes to the host specification.
    ///
    /// This command operates very similarly to `kubectl apply`; if invoked interactively,
    /// then the current host specification will be presented in the system default `$EDITOR`
    /// for interactive changes.
    ///
    /// It is also possible to directly provide new contents via `bootc edit --filename`.
    ///
    /// Only changes to the `spec` section are honored.
    Edit(EditOpts),
    /// Display status
    ///
    /// This will output a YAML-formatted object using a schema intended to match a Kubernetes resource
    /// that describes the state of the booted system.
    ///
    /// The exact API format is not currently declared stable.
    Status(StatusOpts),
    /// Adds a transient writable overlayfs on `/usr` that will be discarded on reboot.
    ///
    /// ## Use cases
    ///
    /// A common pattern is wanting to use tracing/debugging tools, such as `strace`
    /// that may not be in the base image.  A system package manager such as `apt` or
    /// `dnf` can apply changes into this transient overlay that will be discarded on
    /// reboot.
    ///
    /// ## /etc and /var
    ///
    /// However, this command has no effect on `/etc` and `/var` - changes written
    /// there will persist.  It is common for package installations to modify these
    /// directories.
    ///
    /// ## Unmounting
    ///
    /// Almost always, a system process will hold a reference to the open mount point.
    /// You can however invoke `umount -l /usr` to perform a "lazy unmount".
    ///
    #[clap(alias = "usroverlay")]
    UsrOverlay,
    /// Install the running container to a target.
    ///
    /// ## Understanding installations
    ///
    /// OCI containers are effectively layers of tarballs with JSON for metadata; they
    /// cannot be booted directly.  The `bootc install` flow is a highly opinionated
    /// method to take the contents of the container image and install it to a target
    /// block device (or an existing filesystem) in such a way that it can be booted.
    ///
    /// For example, a Linux partition table and filesystem is used, and the bootloader and kernel
    /// embedded in the container image are also prepared.
    ///
    /// A bootc installed container currently uses OSTree as a backend, and this sets
    /// it up such that a subsequent `bootc upgrade` can perform in-place updates.
    ///
    /// An installation is not simply a copy of the container filesystem, but includes
    /// other setup and metadata.
    #[clap(subcommand)]
    #[cfg(feature = "install")]
    Install(InstallOpts),
    /// Operations which can be executed as part of a container build.
    #[clap(subcommand)]
    Container(ContainerOpts),
    /// Operations on container images
    ///
    /// Stability: This interface is not declared stable and may change or be removed
    /// at any point in the future.
    #[clap(subcommand, hide = true)]
    Image(ImageOpts),
    /// Execute the given command in the host mount namespace
    #[cfg(feature = "install")]
    #[clap(hide = true)]
    ExecInHostMountNamespace {
        #[clap(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    #[clap(subcommand)]
    #[clap(hide = true)]
    Internals(InternalsOpts),
    #[clap(hide(true))]
    #[cfg(feature = "docgen")]
    Man(ManOpts),
}

/// Ensure we've entered a mount namespace, so that we can remount
/// `/sysroot` read-write
/// TODO use https://github.com/ostreedev/ostree/pull/2779 once
/// we can depend on a new enough ostree
#[context("Ensuring mountns")]
pub(crate) fn ensure_self_unshared_mount_namespace() -> Result<()> {
    let uid = rustix::process::getuid();
    if !uid.is_root() {
        tracing::debug!("Not root, assuming no need to unshare");
        return Ok(());
    }
    let recurse_env = "_ostree_unshared";
    let ns_pid1 = std::fs::read_link("/proc/1/ns/mnt").context("Reading /proc/1/ns/mnt")?;
    let ns_self = std::fs::read_link("/proc/self/ns/mnt").context("Reading /proc/self/ns/mnt")?;
    // If we already appear to be in a mount namespace, or we're already pid1, we're done
    if ns_pid1 != ns_self {
        tracing::debug!("Already in a mount namespace");
        return Ok(());
    }
    if std::env::var_os(recurse_env).is_some() {
        let am_pid1 = rustix::process::getpid().is_init();
        if am_pid1 {
            tracing::debug!("We are pid 1");
            return Ok(());
        } else {
            anyhow::bail!("Failed to unshare mount namespace");
        }
    }
    crate::reexec::reexec_with_guardenv(recurse_env, &["unshare", "-m", "--"])
}

/// Acquire a locked sysroot.
/// TODO drain this and the above into SysrootLock
#[context("Acquiring sysroot")]
pub(crate) async fn get_locked_sysroot() -> Result<ostree_ext::sysroot::SysrootLock> {
    prepare_for_write()?;
    let sysroot = ostree::Sysroot::new_default();
    sysroot.set_mount_namespace_in_use();
    let sysroot = ostree_ext::sysroot::SysrootLock::new_from_sysroot(&sysroot).await?;
    sysroot.load(gio::Cancellable::NONE)?;
    Ok(sysroot)
}

/// Load global storage state, expecting that we're booted into a bootc system.
#[context("Initializing storage")]
pub(crate) async fn get_storage() -> Result<crate::store::Storage> {
    let global_run = Dir::open_ambient_dir("/run", cap_std::ambient_authority())?;
    let sysroot = get_locked_sysroot().await?;
    crate::store::Storage::new(sysroot, &global_run)
}

#[context("Querying root privilege")]
pub(crate) fn require_root() -> Result<()> {
    let uid = rustix::process::getuid();
    if !uid.is_root() {
        anyhow::bail!("This command requires root privileges");
    }
    if !rustix::thread::capability_is_in_bounding_set(rustix::thread::Capability::SystemAdmin)? {
        anyhow::bail!("This command requires full root privileges (CAP_SYS_ADMIN)");
    }
    tracing::trace!("Verified uid 0 with CAP_SYS_ADMIN");
    Ok(())
}

/// A few process changes that need to be made for writing.
/// IMPORTANT: This may end up re-executing the current process,
/// so anything that happens before this should be idempotent.
#[context("Preparing for write")]
fn prepare_for_write() -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};

    // This is intending to give "at most once" semantics to this
    // function. We should never invoke this from multiple threads
    // at the same time, but verifying "on main thread" is messy.
    // Yes, using SeqCst is likely overkill, but there is nothing perf
    // sensitive about this.
    static ENTERED: AtomicBool = AtomicBool::new(false);
    if ENTERED.load(Ordering::SeqCst) {
        return Ok(());
    }
    if ostree_ext::container_utils::is_ostree_container()? {
        anyhow::bail!(
            "Detected container (ostree base); this command requires a booted host system."
        );
    }
    if ostree_ext::container_utils::running_in_container() {
        anyhow::bail!("Detected container; this command requires a booted host system.");
    }
    if !std::path::Path::new("/run/ostree-booted").exists() {
        anyhow::bail!("This command requires an ostree-booted host system");
    }
    crate::cli::require_root()?;
    ensure_self_unshared_mount_namespace()?;
    if crate::lsm::selinux_enabled()? && !crate::lsm::selinux_ensure_install()? {
        tracing::warn!("Do not have install_t capabilities");
    }
    ENTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Implementation of the `bootc upgrade` CLI command.
#[context("Upgrading")]
async fn upgrade(opts: UpgradeOpts) -> Result<()> {
    let sysroot = &get_storage().await?;
    let repo = &sysroot.repo();
    let (booted_deployment, _deployments, host) =
        crate::status::get_status_require_booted(sysroot)?;
    let imgref = host.spec.image.as_ref();
    // If there's no specified image, let's be nice and check if the booted system is using rpm-ostree
    if imgref.is_none() {
        let booted_incompatible = host
            .status
            .booted
            .as_ref()
            .map_or(false, |b| b.incompatible);

        let staged_incompatible = host
            .status
            .staged
            .as_ref()
            .map_or(false, |b| b.incompatible);

        if booted_incompatible || staged_incompatible {
            return Err(anyhow::anyhow!(
                "Deployment contains local rpm-ostree modifications; cannot upgrade via bootc. You can run `rpm-ostree reset` to undo the modifications."
            ));
        }
    }

    let spec = RequiredHostSpec::from_spec(&host.spec)?;
    let booted_image = host
        .status
        .booted
        .map(|b| b.query_image(repo))
        .transpose()?
        .flatten();
    let imgref = imgref.ok_or_else(|| anyhow::anyhow!("No image source specified"))?;
    // Find the currently queued digest, if any before we pull
    let staged = host.status.staged.as_ref();
    let staged_image = staged.as_ref().and_then(|s| s.image.as_ref());
    let mut changed = false;
    if opts.check {
        let imgref = imgref.clone().into();
        let mut imp = crate::deploy::new_importer(repo, &imgref).await?;
        match imp.prepare().await? {
            PrepareResult::AlreadyPresent(_) => {
                println!("No changes in: {imgref:#}");
            }
            PrepareResult::Ready(r) => {
                crate::deploy::check_bootc_label(&r.config);
                println!("Update available for: {imgref:#}");
                if let Some(version) = r.version() {
                    println!("  Version: {version}");
                }
                println!("  Digest: {}", r.manifest_digest);
                changed = true;
                if let Some(previous_image) = booted_image.as_ref() {
                    let diff =
                        ostree_container::ManifestDiff::new(&previous_image.manifest, &r.manifest);
                    diff.print();
                }
            }
        }
    } else {
        let fetched = crate::deploy::pull(repo, imgref, None, opts.quiet).await?;
        let staged_digest = staged_image.as_ref().map(|s| s.image_digest.as_str());
        let fetched_digest = fetched.manifest_digest.as_str();
        tracing::debug!("staged: {staged_digest:?}");
        tracing::debug!("fetched: {fetched_digest}");
        let staged_unchanged = staged_digest
            .map(|d| d == fetched_digest)
            .unwrap_or_default();
        let booted_unchanged = booted_image
            .as_ref()
            .map(|img| img.manifest_digest.as_str() == fetched_digest)
            .unwrap_or_default();
        if staged_unchanged {
            println!("Staged update present, not changed.");

            if opts.apply {
                crate::reboot::reboot()?;
            }
        } else if booted_unchanged {
            println!("No update available.")
        } else {
            let osname = booted_deployment.osname();
            crate::deploy::stage(sysroot, &osname, &fetched, &spec).await?;
            changed = true;
            if let Some(prev) = booted_image.as_ref() {
                if let Some(fetched_manifest) = fetched.get_manifest(repo)? {
                    let diff =
                        ostree_container::ManifestDiff::new(&prev.manifest, &fetched_manifest);
                    diff.print();
                }
            }
        }
    }
    if changed {
        if opts.apply {
            crate::reboot::reboot()?;
        }
    } else {
        tracing::debug!("No changes");
    }

    Ok(())
}

/// Implementation of the `bootc switch` CLI command.
#[context("Switching")]
async fn switch(opts: SwitchOpts) -> Result<()> {
    let transport = ostree_container::Transport::try_from(opts.transport.as_str())?;
    let imgref = ostree_container::ImageReference {
        transport,
        name: opts.target.to_string(),
    };
    let sigverify = sigpolicy_from_opts(
        !opts.enforce_container_sigpolicy,
        opts.ostree_remote.as_deref(),
    );
    let target = ostree_container::OstreeImageReference { sigverify, imgref };
    let target = ImageReference::from(target);

    // If we're doing an in-place mutation, we shortcut most of the rest of the work here
    if opts.mutate_in_place {
        let deployid = {
            // Clone to pass into helper thread
            let target = target.clone();
            let root = cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
            tokio::task::spawn_blocking(move || {
                crate::deploy::switch_origin_inplace(&root, &target)
            })
            .await??
        };
        println!("Updated {deployid} to pull from {target}");
        return Ok(());
    }

    let cancellable = gio::Cancellable::NONE;

    let sysroot = &get_storage().await?;
    let repo = &sysroot.repo();
    let (booted_deployment, _deployments, host) =
        crate::status::get_status_require_booted(sysroot)?;

    let new_spec = {
        let mut new_spec = host.spec.clone();
        new_spec.image = Some(target.clone());
        new_spec
    };

    if new_spec == host.spec {
        println!("Image specification is unchanged.");
        return Ok(());
    }
    let new_spec = RequiredHostSpec::from_spec(&new_spec)?;

    let fetched = crate::deploy::pull(repo, &target, None, opts.quiet).await?;

    if !opts.retain {
        // By default, we prune the previous ostree ref so it will go away after later upgrades
        if let Some(booted_origin) = booted_deployment.origin() {
            if let Some(ostree_ref) = booted_origin.optional_string("origin", "refspec")? {
                let (remote, ostree_ref) =
                    ostree::parse_refspec(&ostree_ref).context("Failed to parse ostree ref")?;
                repo.set_ref_immediate(remote.as_deref(), &ostree_ref, None, cancellable)?;
            }
        }
    }

    let stateroot = booted_deployment.osname();
    crate::deploy::stage(sysroot, &stateroot, &fetched, &new_spec).await?;

    Ok(())
}

/// Implementation of the `bootc rollback` CLI command.
#[context("Rollback")]
async fn rollback(_opts: RollbackOpts) -> Result<()> {
    let sysroot = &get_storage().await?;
    crate::deploy::rollback(sysroot).await
}

/// Implementation of the `bootc edit` CLI command.
#[context("Editing spec")]
async fn edit(opts: EditOpts) -> Result<()> {
    let sysroot = &get_storage().await?;
    let repo = &sysroot.repo();

    let (booted_deployment, _deployments, host) =
        crate::status::get_status_require_booted(sysroot)?;
    let new_host: Host = if let Some(filename) = opts.filename {
        let mut r = std::io::BufReader::new(std::fs::File::open(filename)?);
        serde_yaml::from_reader(&mut r)?
    } else {
        let tmpf = tempfile::NamedTempFile::new()?;
        serde_yaml::to_writer(std::io::BufWriter::new(tmpf.as_file()), &host)?;
        crate::utils::spawn_editor(&tmpf)?;
        tmpf.as_file().seek(std::io::SeekFrom::Start(0))?;
        serde_yaml::from_reader(&mut tmpf.as_file())?
    };

    if new_host.spec == host.spec {
        println!("Edit cancelled, no changes made.");
        return Ok(());
    }
    host.spec.verify_transition(&new_host.spec)?;
    let new_spec = RequiredHostSpec::from_spec(&new_host.spec)?;

    // We only support two state transitions right now; switching the image,
    // or flipping the bootloader ordering.
    if host.spec.boot_order != new_host.spec.boot_order {
        return crate::deploy::rollback(sysroot).await;
    }

    let fetched = crate::deploy::pull(repo, new_spec.image, None, opts.quiet).await?;

    // TODO gc old layers here

    let stateroot = booted_deployment.osname();
    crate::deploy::stage(sysroot, &stateroot, &fetched, &new_spec).await?;

    Ok(())
}

/// Implementation of `bootc usroverlay`
async fn usroverlay() -> Result<()> {
    // This is just a pass-through today.  At some point we may make this a libostree API
    // or even oxidize it.
    return Err(Command::new("ostree")
        .args(["admin", "unlock"])
        .exec()
        .into());
}

/// Perform process global initialization. This should be called as early as possible
/// in the standard `main` function.
pub fn global_init() -> Result<()> {
    let am_root = rustix::process::getuid().is_root();
    // Work around bootc-image-builder not setting HOME, in combination with podman (really c/common)
    // bombing out if it is unset.
    if std::env::var_os("HOME").is_none() && am_root {
        // Setting the environment is thread-unsafe, but we ask calling code
        // to invoke this as early as possible. (In practice, that's just the cli's `main.rs`)
        // xref https://internals.rust-lang.org/t/synchronized-ffi-access-to-posix-environment-variable-functions/15475
        std::env::set_var("HOME", "/root");
    }
    Ok(())
}

/// Parse the provided arguments and execute.
/// Calls [`structopt::clap::Error::exit`] on failure, printing the error message and aborting the program.
pub async fn run_from_iter<I>(args: I) -> Result<()>
where
    I: IntoIterator,
    I::Item: Into<OsString> + Clone,
{
    run_from_opt(Opt::parse_including_static(args)).await
}

impl Opt {
    /// In some cases (e.g. systemd generator) we dispatch specifically on argv0.  This
    /// requires some special handling in clap.
    fn parse_including_static<I>(args: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<OsString> + Clone,
    {
        let mut args = args.into_iter();
        let first = if let Some(first) = args.next() {
            let first: OsString = first.into();
            let argv0 = first.to_str().and_then(|s| s.rsplit_once('/')).map(|s| s.1);
            tracing::debug!("argv0={argv0:?}");
            if matches!(argv0, Some(InternalsOpts::GENERATOR_BIN)) {
                let base_args = ["bootc", "internals", "systemd-generator"]
                    .into_iter()
                    .map(OsString::from);
                return Opt::parse_from(base_args.chain(args.map(|i| i.into())));
            }
            Some(first)
        } else {
            None
        };
        Opt::parse_from(first.into_iter().chain(args.map(|i| i.into())))
    }
}

/// Internal (non-generic/monomorphized) primary CLI entrypoint
async fn run_from_opt(opt: Opt) -> Result<()> {
    let root = &Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    match opt {
        Opt::Upgrade(opts) => upgrade(opts).await,
        Opt::Switch(opts) => switch(opts).await,
        Opt::Rollback(opts) => rollback(opts).await,
        Opt::Edit(opts) => edit(opts).await,
        Opt::UsrOverlay => usroverlay().await,
        Opt::Container(opts) => match opts {
            ContainerOpts::Lint => {
                if !ostree_ext::container_utils::is_ostree_container()? {
                    anyhow::bail!(
                        "Not in a ostree container, this command only verifies ostree containers."
                    );
                }

                lints::lint(root)?;
                Ok(())
            }
        },
        Opt::Image(opts) => match opts {
            ImageOpts::List => crate::image::list_entrypoint().await,
            ImageOpts::CopyToStorage { source, target } => {
                crate::image::push_entrypoint(source.as_deref(), target.as_deref()).await
            }
        },
        #[cfg(feature = "install")]
        Opt::Install(opts) => match opts {
            InstallOpts::ToDisk(opts) => crate::install::install_to_disk(opts).await,
            InstallOpts::ToFilesystem(opts) => {
                crate::install::install_to_filesystem(opts, false).await
            }
            InstallOpts::ToExistingRoot(opts) => {
                crate::install::install_to_existing_root(opts).await
            }
            InstallOpts::PrintConfiguration => crate::install::print_configuration(),
        },
        #[cfg(feature = "install")]
        Opt::ExecInHostMountNamespace { args } => {
            crate::install::exec_in_host_mountns(args.as_slice())
        }
        Opt::Status(opts) => super::status::status(opts).await,
        Opt::Internals(opts) => match opts {
            InternalsOpts::SystemdGenerator {
                normal_dir,
                early_dir: _,
                late_dir: _,
            } => {
                let unit_dir = &Dir::open_ambient_dir(normal_dir, cap_std::ambient_authority())?;
                crate::generator::generator(root, unit_dir)
            }
            InternalsOpts::FixupEtcFstab => crate::deploy::fixup_etc_fstab(&root),
            InternalsOpts::PrintJsonSchema => {
                let schema = schema_for!(crate::spec::Host);
                let mut stdout = std::io::stdout().lock();
                serde_json::to_writer_pretty(&mut stdout, &schema)?;
                Ok(())
            }
        },
        #[cfg(feature = "docgen")]
        Opt::Man(manopts) => crate::docgen::generate_manpages(&manopts.directory),
    }
}

#[test]
fn test_parse_install_args() {
    // Verify we still process the legacy --target-no-signature-verification
    let o = Opt::try_parse_from([
        "bootc",
        "install",
        "to-filesystem",
        "--target-no-signature-verification",
        "/target",
    ])
    .unwrap();
    let o = match o {
        Opt::Install(InstallOpts::ToFilesystem(fsopts)) => fsopts,
        o => panic!("Expected filesystem opts, not {o:?}"),
    };
    assert!(o.target_opts.target_no_signature_verification);
    assert_eq!(o.filesystem_opts.root_path.as_str(), "/target");
}

#[test]
fn test_parse_opts() {
    assert!(matches!(
        Opt::parse_including_static(["bootc", "status"]),
        Opt::Status(StatusOpts {
            json: false,
            format: None,
            format_version: None,
            booted: false
        })
    ));
    assert!(matches!(
        Opt::parse_including_static(["bootc", "status", "--format-version=0"]),
        Opt::Status(StatusOpts {
            format_version: Some(0),
            ..
        })
    ));
}

#[test]
fn test_parse_generator() {
    assert!(matches!(
        Opt::parse_including_static([
            "/usr/lib/systemd/system/bootc-systemd-generator",
            "/run/systemd/system"
        ]),
        Opt::Internals(InternalsOpts::SystemdGenerator { .. })
    ));
}
