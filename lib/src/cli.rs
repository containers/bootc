//! # Bootable container image CLI
//!
//! Command line tool to manage bootable ostree-based containers.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use clap::Parser;
use ostree::{gio, glib};
use ostree_container::store::LayeredImageState;
use ostree_container::store::PrepareResult;
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::container::SignatureSource;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;
use ostree_ext::sysroot::SysrootLock;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;

/// Perform an upgrade operation
#[derive(Debug, Parser)]
pub(crate) struct UpgradeOpts {
    /// Don't display progress
    #[clap(long)]
    pub(crate) quiet: bool,

    #[clap(long)]
    pub(crate) touch_if_changed: Option<Utf8PathBuf>,
}

/// Perform an upgrade operation
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

/// Perform an upgrade operation
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

/// Deploy and upgrade via bootable container images.
#[derive(Debug, Parser)]
#[clap(name = "bootc")]
#[clap(rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Opt {
    /// Look for updates to the booted container image.
    Upgrade(UpgradeOpts),
    /// Target a new container image reference to boot.
    Switch(SwitchOpts),
    /// Display status
    Status(StatusOpts),
    #[clap(hide(true))]
    #[cfg(feature = "docgen")]
    Man(ManOpts),
}

/// Ensure we've entered a mount namespace, so that we can remount
/// `/sysroot` read-write
/// TODO use https://github.com/ostreedev/ostree/pull/2779 once
/// we can depend on a new enough ostree
async fn ensure_self_unshared_mount_namespace() -> Result<()> {
    let uid = cap_std_ext::rustix::process::getuid();
    if !uid.is_root() {
        return Ok(());
    }
    let recurse_env = "_ostree_unshared";
    let ns_pid1 = std::fs::read_link("/proc/1/ns/mnt").context("Reading /proc/1/ns/mnt")?;
    let ns_self = std::fs::read_link("/proc/self/ns/mnt").context("Reading /proc/self/ns/mnt")?;
    // If we already appear to be in a mount namespace, we're done
    if ns_pid1 != ns_self {
        return Ok(());
    }
    if std::env::var_os(recurse_env).is_some() {
        anyhow::bail!("Failed to unshare mount namespace");
    }
    let self_exe = std::fs::read_link("/proc/self/exe")?;
    let mut cmd = std::process::Command::new("unshare");
    cmd.env(recurse_env, "1");
    cmd.args(["-m", "--"])
        .arg(self_exe)
        .args(std::env::args_os().skip(1));
    Err(cmd.exec().into())
}

/// Acquire a locked sysroot.
/// TODO drain this and the above into SysrootLock
pub(crate) async fn get_locked_sysroot() -> Result<ostree_ext::sysroot::SysrootLock> {
    let sysroot = ostree::Sysroot::new_default();
    sysroot.set_mount_namespace_in_use();
    let sysroot = ostree_ext::sysroot::SysrootLock::new_from_sysroot(&sysroot).await?;
    sysroot.load(gio::Cancellable::NONE)?;
    Ok(sysroot)
}

/// Wrapper for pulling a container image, wiring up status output.
async fn pull(
    repo: &ostree::Repo,
    imgref: &OstreeImageReference,
    quiet: bool,
) -> Result<Box<LayeredImageState>> {
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

/// Stage (queue deployment of) a fetched container image.
async fn stage(
    sysroot: &SysrootLock,
    stateroot: &str,
    imgref: &ostree_container::OstreeImageReference,
    image: Box<LayeredImageState>,
    origin: &glib::KeyFile,
) -> Result<()> {
    let cancellable = gio::Cancellable::NONE;
    let stateroot = Some(stateroot);
    let merge_deployment = sysroot.merge_deployment(stateroot);
    let _new_deployment = sysroot.stage_tree_with_options(
        stateroot,
        image.merge_commit.as_str(),
        Some(origin),
        merge_deployment.as_ref(),
        &Default::default(),
        cancellable,
    )?;
    println!("Queued for next boot: {imgref}");
    Ok(())
}

/// A few process changes that need to be made for writing.
async fn prepare_for_write() -> Result<()> {
    ensure_self_unshared_mount_namespace().await?;
    ostree_ext::selinux::verify_install_domain()?;
    Ok(())
}

/// Implementation of the `bootc upgrade` CLI command.
async fn upgrade(opts: UpgradeOpts) -> Result<()> {
    prepare_for_write().await?;
    let sysroot = &get_locked_sysroot().await?;
    let repo = &sysroot.repo().unwrap();
    let booted_deployment = &sysroot.require_booted_deployment()?;
    let status = crate::status::DeploymentStatus::from_deployment(booted_deployment, true)?;
    let osname = booted_deployment.osname().unwrap();
    let origin = booted_deployment
        .origin()
        .ok_or_else(|| anyhow::anyhow!("Deployment is missing an origin"))?;
    let imgref = status
        .image
        .ok_or_else(|| anyhow::anyhow!("Booted deployment is not container image based"))?;
    let imgref: OstreeImageReference = imgref.into();
    if !status.supported {
        return Err(anyhow::anyhow!(
            "Booted deployment contains local rpm-ostree modifications; cannot upgrade via bootc"
        ));
    }
    let commit = booted_deployment.csum().unwrap();
    let state = ostree_container::store::query_image_commit(repo, &commit)?;
    let digest = state.manifest_digest.as_str();
    let fetched = pull(repo, &imgref, opts.quiet).await?;

    if fetched.merge_commit.as_str() == commit.as_str() {
        println!("Already queued: {digest}");
        return Ok(());
    }

    stage(sysroot, &osname, &imgref, fetched, &origin).await?;

    if let Some(path) = opts.touch_if_changed {
        std::fs::write(&path, "").with_context(|| format!("Writing {path}"))?;
    }

    Ok(())
}

/// Implementation of the `bootc switch` CLI command.
async fn switch(opts: SwitchOpts) -> Result<()> {
    prepare_for_write().await?;

    let cancellable = gio::Cancellable::NONE;
    let sysroot = get_locked_sysroot().await?;
    let booted_deployment = &sysroot.require_booted_deployment()?;
    let (origin, booted_image) = crate::utils::get_image_origin(booted_deployment)?;
    let booted_refspec = origin.optional_string("origin", "refspec")?;
    let osname = booted_deployment.osname().unwrap();
    let repo = &sysroot.repo().unwrap();

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

    let fetched = pull(repo, &target, opts.quiet).await?;

    if !opts.retain {
        // By default, we prune the previous ostree ref or container image
        if let Some(ostree_ref) = booted_refspec {
            let (remote, ostree_ref) =
                ostree::parse_refspec(&ostree_ref).context("Failed to parse ostree ref")?;
            repo.set_ref_immediate(remote.as_deref(), &ostree_ref, None, cancellable)?;
            origin.remove_key("origin", "refspec")?;
        } else if let Some(booted_image) = booted_image.as_ref() {
            ostree_container::store::remove_image(repo, &booted_image.imgref)?;
            let _nlayers: u32 = ostree_container::store::gc_image_layers(repo)?;
        }
    }

    // We always make a fresh origin to toss out old state.
    let origin = glib::KeyFile::new();
    origin.set_string(
        "origin",
        ostree_container::deploy::ORIGIN_CONTAINER,
        target.to_string().as_str(),
    );
    stage(&sysroot, &osname, &target, fetched, &origin).await?;

    Ok(())
}

/// Parse the provided arguments and execute.
/// Calls [`structopt::clap::Error::exit`] on failure, printing the error message and aborting the program.
pub async fn run_from_iter<I>(args: I) -> Result<()>
where
    I: IntoIterator,
    I::Item: Into<OsString> + Clone,
{
    let opt = Opt::parse_from(args);
    match opt {
        Opt::Upgrade(opts) => upgrade(opts).await,
        Opt::Switch(opts) => switch(opts).await,
        Opt::Status(opts) => super::status::status(opts).await,
        #[cfg(feature = "docgen")]
        Opt::Man(manopts) => crate::docgen::generate_manpages(&manopts.directory),
    }
}
