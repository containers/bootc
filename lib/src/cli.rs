//! # Bootable container image CLI
//!
//! Command line tool to manage bootable ostree-based containers.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use clap::Parser;
use fn_error_context::context;
use ostree::gio;
use ostree_container::store::LayeredImageState;
use ostree_container::store::PrepareResult;
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::container::SignatureSource;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;
use std::ffi::OsString;
use std::io::Seek;
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::spec::Host;
use crate::spec::ImageReference;

/// Perform an upgrade operation
#[derive(Debug, Parser)]
pub(crate) struct UpgradeOpts {
    /// Don't display progress
    #[clap(long)]
    pub(crate) quiet: bool,

    #[clap(long)]
    pub(crate) touch_if_changed: Option<Utf8PathBuf>,

    /// Check if an update is available without applying it
    #[clap(long)]
    pub(crate) check: bool,
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

    /// Explicitly opt-out of requiring any form of signature verification.
    #[clap(long)]
    pub(crate) no_signature_verification: bool,

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
    /// Path to new system specification; use `-` for stdin
    pub(crate) filename: String,

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
    /// e2e test of install-to-filesystem
    TestInstallFilesystem {
        image: String,
        blockdev: Utf8PathBuf,
    },
}

/// Deploy and upgrade via bootable container images.
#[derive(Debug, Parser)]
#[clap(name = "bootc")]
#[clap(rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Opt {
    /// Look for updates to the booted container image.
    #[clap(alias = "update")]
    Upgrade(UpgradeOpts),
    /// Target a new container image reference to boot.
    Switch(SwitchOpts),
    /// Change host specification
    Edit(EditOpts),
    /// Display status
    Status(StatusOpts),
    /// Add a transient writable overlayfs on `/usr` that will be discarded on reboot.
    #[clap(alias = "usroverlay")]
    UsrOverlay,
    /// Install to the target block device
    #[cfg(feature = "install")]
    Install(crate::install::InstallOpts),
    /// Install to the target filesystem.
    #[cfg(feature = "install")]
    InstallToFilesystem(crate::install::InstallToFilesystemOpts),
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

/// Wrapper for pulling a container image, wiring up status output.
#[context("Pulling")]
async fn pull(
    repo: &ostree::Repo,
    imgref: &ImageReference,
    quiet: bool,
) -> Result<Box<LayeredImageState>> {
    let imgref = &OstreeImageReference::from(imgref.clone());
    let config = Default::default();
    let mut imp = ostree_container::store::ImageImporter::new(repo, imgref, config).await?;
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => {
            println!("No changes in {} => {}", imgref, c.manifest_digest);
            return Ok(c);
        }
        PrepareResult::Ready(p) => p,
    };
    if let Some(warning) = prep.deprecated_warning() {
        ostree_ext::cli::print_deprecated_warning(warning).await;
    }
    ostree_ext::cli::print_layer_status(&prep);
    let printer = (!quiet).then(|| {
        let layer_progress = imp.request_progress();
        let layer_byte_progress = imp.request_layer_progress();
        tokio::task::spawn(async move {
            ostree_ext::cli::handle_layer_progress_print(layer_progress, layer_byte_progress).await
        })
    });
    let import = imp.import(prep).await;
    if let Some(printer) = printer {
        let _ = printer.await;
    }
    let import = import?;
    if let Some(msg) =
        ostree_container::store::image_filtered_content_warning(repo, &imgref.imgref)?
    {
        eprintln!("{msg}")
    }
    Ok(import)
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
    let booted_deployment = &sysroot.require_booted_deployment()?;
    let (_deployments, host) = crate::status::get_status(sysroot, Some(booted_deployment))?;
    // SAFETY: There must be a status if we have a booted deployment
    let status = host.status.unwrap();
    let imgref = host.spec.image.as_ref();
    // If there's no specified image, let's be nice and check if the booted system is using rpm-ostree
    if imgref.is_none() && status.booted.map_or(false, |b| b.incompatible) {
        return Err(anyhow::anyhow!(
            "Booted deployment contains local rpm-ostree modifications; cannot upgrade via bootc"
        ));
    }
    let imgref = imgref.ok_or_else(|| anyhow::anyhow!("No image source specified"))?;
    // Find the currently queued digest, if any before we pull
    let queued_digest = status
        .staged
        .as_ref()
        .and_then(|e| e.image.as_ref())
        .map(|img| img.image_digest.as_str());
    if opts.check {
        // pull the image manifest without the layers
        let config = Default::default();
        let imgref = &OstreeImageReference::from(imgref.clone());
        let mut imp =
            ostree_container::store::ImageImporter::new(&sysroot.repo(), imgref, config).await?;
        imp.require_bootable();
        match imp.prepare().await? {
            PrepareResult::AlreadyPresent(c) => {
                println!(
                    "No changes available for {}. Latest digest: {}",
                    imgref, c.manifest_digest
                );
                return Ok(());
            }
            PrepareResult::Ready(r) => {
                // TODO show a diff
                println!(
                    "New image available for {imgref}. Digest {}",
                    r.manifest_digest
                );
                // Note here we'll fall through to handling the --touch-if-changed below
            }
        }
    } else {
        let fetched = pull(&sysroot.repo(), imgref, opts.quiet).await?;
        if let Some(queued_digest) = queued_digest {
            if fetched.merge_commit.as_str() == queued_digest {
                println!("Already queued: {queued_digest}");
                return Ok(());
            }
        }

        let osname = booted_deployment.osname();
        crate::deploy::stage(sysroot, &osname, fetched, &host.spec).await?;
    }
    if let Some(path) = opts.touch_if_changed {
        std::fs::write(&path, "").with_context(|| format!("Writing {path}"))?;
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
    let booted_deployment = &sysroot.require_booted_deployment()?;
    let (_deployments, host) = crate::status::get_status(sysroot, Some(booted_deployment))?;

    let transport = ostree_container::Transport::try_from(opts.transport.as_str())?;
    let imgref = ostree_container::ImageReference {
        transport,
        name: opts.target.to_string(),
    };
    let sigverify = if opts.no_signature_verification {
        SignatureSource::ContainerPolicyAllowInsecure
    } else if let Some(remote) = opts.ostree_remote.as_ref() {
        SignatureSource::OstreeRemote(remote.to_string())
    } else {
        SignatureSource::ContainerPolicy
    };
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

    let fetched = pull(repo, &target, opts.quiet).await?;

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
    crate::deploy::stage(sysroot, &stateroot, fetched, &new_spec).await?;

    Ok(())
}

/// Implementation of the `bootc edit` CLI command.
#[context("Editing spec")]
async fn edit(opts: EditOpts) -> Result<()> {
    prepare_for_write().await?;
    let sysroot = &get_locked_sysroot().await?;
    let repo = &sysroot.repo();
    let booted_deployment = &sysroot.require_booted_deployment()?;
    let (_deployments, host) = crate::status::get_status(sysroot, Some(booted_deployment))?;

    let new_host: Host = if opts.filename == "-" {
        let tmpf = tempfile::NamedTempFile::new()?;
        serde_yaml::to_writer(std::io::BufWriter::new(tmpf.as_file()), &host)?;
        crate::utils::spawn_editor(&tmpf)?;
        tmpf.as_file().seek(std::io::SeekFrom::Start(0))?;
        serde_yaml::from_reader(&mut tmpf.as_file())?
    } else {
        let mut r = std::io::BufReader::new(std::fs::File::open(opts.filename)?);
        serde_yaml::from_reader(&mut r)?
    };

    if new_host.spec == host.spec {
        anyhow::bail!("No changes in current host spec");
    }
    let new_image = new_host
        .spec
        .image
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Unable to transition to unset image"))?;
    let fetched = pull(repo, new_image, opts.quiet).await?;

    // TODO gc old layers here

    let stateroot = booted_deployment.osname();
    crate::deploy::stage(sysroot, &stateroot, fetched, &new_host.spec).await?;

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
        Opt::Install(opts) => crate::install::install(opts).await,
        #[cfg(feature = "install")]
        Opt::InstallToFilesystem(opts) => crate::install::install_to_filesystem(opts).await,
        Opt::Status(opts) => super::status::status(opts).await,
        #[cfg(feature = "internal-testing-api")]
        Opt::InternalTests(opts) => crate::privtests::run(opts).await,
        #[cfg(feature = "docgen")]
        Opt::Man(manopts) => crate::docgen::generate_manpages(&manopts.directory),
    }
}
