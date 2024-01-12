//! # Bootable container image CLI
//!
//! Command line tool to manage bootable ostree-based containers.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use clap::Parser;
use fn_error_context::context;
use ostree::gio;
use ostree_container::store::PrepareResult;
use ostree_ext::container as ostree_container;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;
use std::ffi::OsString;
use std::io::Seek;
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::deploy::RequiredHostSpec;
use crate::spec::Host;
use crate::spec::ImageReference;
use crate::utils::sigpolicy_from_opts;

/// Perform an upgrade operation
#[derive(Debug, Parser)]
pub(crate) struct UpgradeOpts {
    /// Don't display progress
    #[clap(long)]
    pub(crate) quiet: bool,

    #[clap(long)]
    pub(crate) touch_if_changed: Option<Utf8PathBuf>,

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
#[derive(Debug, Parser)]
pub(crate) struct SwitchOpts {
    /// Don't display progress
    #[clap(long)]
    pub(crate) quiet: bool,

    /// The transport; e.g. oci, oci-archive.  Defaults to `registry`.
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

    /// Retain reference to currently booted image
    #[clap(long)]
    pub(crate) retain: bool,

    /// Target image to use for the next boot.
    pub(crate) target: String,
}

/// Perform an edit operation
#[derive(Debug, Parser)]
pub(crate) struct EditOpts {
    /// Use filename to edit system specification
    #[clap(long, short = 'f')]
    pub(crate) filename: Option<String>,

    /// Don't display progress
    #[clap(long)]
    pub(crate) quiet: bool,
}

/// Perform an status operation
#[derive(Debug, Parser)]
pub(crate) struct StatusOpts {
    /// Output in JSON format.
    #[clap(long)]
    pub(crate) json: bool,

    /// Only display status for the booted deployment.
    #[clap(long)]
    pub(crate) booted: bool,
}

/// Options for internal testing
#[cfg(feature = "install")]
#[derive(Debug, clap::Subcommand)]
pub(crate) enum InstallOpts {
    /// Install to the target block device
    ToDisk(crate::install::InstallToDiskOpts),
    /// Install to the target filesystem
    ToFilesystem(crate::install::InstallToFilesystemOpts),
}

/// Options for man page generation
#[derive(Debug, Parser)]
pub(crate) struct ManOpts {
    #[clap(long)]
    /// Output to this directory
    pub(crate) directory: Utf8PathBuf,
}

/// Options for internal testing
#[derive(Debug, clap::Subcommand)]
pub(crate) enum TestingOpts {
    /// Execute integration tests that require a privileged container
    RunPrivilegedIntegration {},
    /// Execute integration tests that target a not-privileged ostree container
    RunContainerIntegration {},
    /// Block device setup for testing
    PrepTestInstallFilesystem { blockdev: Utf8PathBuf },
    /// e2e test of install to-filesystem
    TestInstallFilesystem {
        image: String,
        blockdev: Utf8PathBuf,
    },
}

/// Deploy and transactionally in-place with bootable container images.
///
/// The `bootc` project currently uses ostree-containers as a backend
/// to support a model of bootable container images.  Once installed,
/// whether directly via `bootc install` (executed as part of a container)
/// or via another mechanism such as an OS installer tool, further
/// updates can be pulled via e.g. `bootc upgrade`.
///
/// Changes in `/etc` and `/var` persist.
///
#[derive(Debug, Parser)]
#[clap(name = "bootc")]
#[clap(rename_all = "kebab-case")]
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
    /// do *not* upgrade.
    #[clap(alias = "update")]
    Upgrade(UpgradeOpts),
    /// Target a new container image reference to boot.
    ///
    /// This operates in a very similar fashion to `upgrade`, but changes the container image reference
    /// instead.
    Switch(SwitchOpts),
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
    /// Add a transient writable overlayfs on `/usr` that will be discarded on reboot.
    #[clap(alias = "usroverlay")]
    UsrOverlay,
    /// Install the running container to a target.
    ///
    /// This has two main sub-commands `to-disk` (which expects an empty block device) and `to-filesystem`
    /// which supports installation to an already extant filesystem.
    #[clap(subcommand)]
    #[cfg(feature = "install")]
    Install(InstallOpts),
    /// Execute the given command in the host mount namespace
    #[cfg(feature = "install")]
    #[clap(hide = true)]
    ExecInHostMountNamespace {
        #[clap(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Internal integration testing helpers.
    #[clap(hide(true), subcommand)]
    #[cfg(feature = "internal-testing-api")]
    InternalTests(TestingOpts),
    #[clap(hide(true))]
    #[cfg(feature = "docgen")]
    Man(ManOpts),
}

/// Ensure we've entered a mount namespace, so that we can remount
/// `/sysroot` read-write
/// TODO use https://github.com/ostreedev/ostree/pull/2779 once
/// we can depend on a new enough ostree
#[context("Ensuring mountns")]
pub(crate) async fn ensure_self_unshared_mount_namespace() -> Result<()> {
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
    let sysroot = ostree::Sysroot::new_default();
    sysroot.set_mount_namespace_in_use();
    let sysroot = ostree_ext::sysroot::SysrootLock::new_from_sysroot(&sysroot).await?;
    sysroot.load(gio::Cancellable::NONE)?;
    Ok(sysroot)
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
    Ok(())
}

/// A few process changes that need to be made for writing.
#[context("Preparing for write")]
pub(crate) async fn prepare_for_write() -> Result<()> {
    crate::cli::require_root()?;
    if ostree_ext::container_utils::is_ostree_container()? {
        anyhow::bail!(
            "Detected container (ostree base); this command requires a booted host system."
        );
    }
    ensure_self_unshared_mount_namespace().await?;
    if crate::lsm::selinux_enabled()? {
        crate::lsm::selinux_ensure_install()?;
    }
    Ok(())
}

/// Implementation of the `bootc upgrade` CLI command.
#[context("Upgrading")]
async fn upgrade(opts: UpgradeOpts) -> Result<()> {
    prepare_for_write().await?;
    let sysroot = &get_locked_sysroot().await?;
    let repo = &sysroot.repo();
    let (booted_deployment, _deployments, host) =
        crate::status::get_status_require_booted(sysroot)?;
    let imgref = host.spec.image.as_ref();
    // If there's no specified image, let's be nice and check if the booted system is using rpm-ostree
    if imgref.is_none()
        && host
            .status
            .booted
            .as_ref()
            .map_or(false, |b| b.incompatible)
    {
        return Err(anyhow::anyhow!(
            "Booted deployment contains local rpm-ostree modifications; cannot upgrade via bootc"
        ));
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
                println!("No changes in: {}", imgref);
            }
            PrepareResult::Ready(r) => {
                println!("Update available for: {}", imgref);
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
        let fetched = crate::deploy::pull(sysroot, imgref, opts.quiet).await?;
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
        if let Some(path) = opts.touch_if_changed {
            std::fs::write(&path, "").with_context(|| format!("Writing {path}"))?;
        }
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
    prepare_for_write().await?;
    let cancellable = gio::Cancellable::NONE;

    let sysroot = &get_locked_sysroot().await?;
    let repo = &sysroot.repo();
    let (booted_deployment, _deployments, host) =
        crate::status::get_status_require_booted(sysroot)?;

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

    let new_spec = {
        let mut new_spec = host.spec.clone();
        new_spec.image = Some(target.clone());
        new_spec
    };

    if new_spec == host.spec {
        anyhow::bail!("No changes in current host spec");
    }
    let new_spec = RequiredHostSpec::from_spec(&new_spec)?;

    let fetched = crate::deploy::pull(sysroot, &target, opts.quiet).await?;

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

/// Implementation of the `bootc edit` CLI command.
#[context("Editing spec")]
async fn edit(opts: EditOpts) -> Result<()> {
    prepare_for_write().await?;
    let sysroot = &get_locked_sysroot().await?;
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
    let new_spec = RequiredHostSpec::from_spec(&new_host.spec)?;
    let fetched = crate::deploy::pull(sysroot, new_spec.image, opts.quiet).await?;

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

/// Parse the provided arguments and execute.
/// Calls [`structopt::clap::Error::exit`] on failure, printing the error message and aborting the program.
pub async fn run_from_iter<I>(args: I) -> Result<()>
where
    I: IntoIterator,
    I::Item: Into<OsString> + Clone,
{
    run_from_opt(Opt::parse_from(args)).await
}

/// Internal (non-generic/monomorphized) primary CLI entrypoint
async fn run_from_opt(opt: Opt) -> Result<()> {
    match opt {
        Opt::Upgrade(opts) => upgrade(opts).await,
        Opt::Switch(opts) => switch(opts).await,
        Opt::Edit(opts) => edit(opts).await,
        Opt::UsrOverlay => usroverlay().await,
        #[cfg(feature = "install")]
        Opt::Install(opts) => match opts {
            InstallOpts::ToDisk(opts) => crate::install::install_to_disk(opts).await,
            InstallOpts::ToFilesystem(opts) => crate::install::install_to_filesystem(opts).await,
        },
        #[cfg(feature = "install")]
        Opt::ExecInHostMountNamespace { args } => {
            crate::install::exec_in_host_mountns(args.as_slice())
        }
        Opt::Status(opts) => super::status::status(opts).await,
        #[cfg(feature = "internal-testing-api")]
        Opt::InternalTests(opts) => crate::privtests::run(opts).await,
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
