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
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;
use std::ffi::OsString;
use std::ops::Deref;
use std::os::unix::process::CommandExt;
use tokio::sync::mpsc::Receiver;

/// Perform an upgrade operation
#[derive(Debug, Parser)]
pub(crate) struct UpgradeOpts {
    /// Don't display progress
    #[clap(long)]
    quiet: bool,

    #[clap(long)]
    touch_if_changed: Option<Utf8PathBuf>,
}

/// Perform an upgrade operation
#[derive(Debug, Parser)]
pub(crate) struct SwitchOpts {
    /// Don't display progress
    #[clap(long)]
    quiet: bool,

    /// The transport; e.g. oci, oci-archive.  Defaults to `registry`.
    #[clap(long, default_value = "registry")]
    transport: String,

    /// Explicitly opt-out of requiring any form of signature verification.
    #[clap(long)]
    no_signature_verification: bool,

    /// Enable verification via an ostree remote
    #[clap(long)]
    ostree_remote: Option<String>,

    /// Retain reference to currently booted image
    #[clap(long)]
    retain: bool,

    /// Target image to use for the next boot.
    target: String,
}

/// Perform an upgrade operation
#[derive(Debug, Parser)]
pub(crate) struct StatusOpts {
    /// Output in JSON format.
    #[clap(long)]
    json: bool,

    /// Only display status for the booted deployment.
    #[clap(long)]
    booted: bool,
}

/// Options for man page generation
#[derive(Debug, Parser)]
pub(crate) struct ManOpts {
    #[clap(long)]
    /// Output to this directory
    directory: Utf8PathBuf,
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

async fn get_locked_sysroot() -> Result<ostree_ext::sysroot::SysrootLock> {
    let sysroot = ostree::Sysroot::new_default();
    sysroot.set_mount_namespace_in_use();
    let sysroot = ostree_ext::sysroot::SysrootLock::new_from_sysroot(&sysroot).await?;
    sysroot.load(gio::Cancellable::NONE)?;
    Ok(sysroot)
}

pub(crate) async fn handle_layer_progress_print(
    mut layers: Receiver<ostree_container::store::ImportProgress>,
    mut layer_bytes: tokio::sync::watch::Receiver<Option<ostree_container::store::LayerProgress>>,
) {
    let style = indicatif::ProgressStyle::default_bar();
    let pb = indicatif::ProgressBar::new(100);
    pb.set_style(
        style
            .template("{prefix} {bytes} [{bar:20}] ({eta}) {msg}")
            .unwrap(),
    );
    loop {
        tokio::select! {
            // Always handle layer changes first.
            biased;
            layer = layers.recv() => {
                if let Some(l) = layer {
                    if l.is_starting() {
                        pb.set_position(0);
                    } else {
                        pb.finish();
                    }
                    pb.set_message(ostree_ext::cli::layer_progress_format(&l));
                } else {
                    // If the receiver is disconnected, then we're done
                    break
                };
            },
            r = layer_bytes.changed() => {
                if r.is_err() {
                    // If the receiver is disconnected, then we're done
                    break
                }
                let bytes = layer_bytes.borrow();
                if let Some(bytes) = &*bytes {
                    pb.set_length(bytes.total);
                    pb.set_position(bytes.fetched);
                }
            }

        }
    }
}

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
        crate::cli::print_deprecated_warning(warning);
    }
    crate::cli::print_layer_status(&prep);
    let printer = (!quiet).then(|| {
        let layer_progress = imp.request_progress();
        let layer_byte_progress = imp.request_layer_progress();
        tokio::task::spawn(async move {
            handle_layer_progress_print(layer_progress, layer_byte_progress).await
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

fn get_image_origin(
    deployment: &ostree::Deployment,
) -> Result<(glib::KeyFile, Option<OstreeImageReference>)> {
    let origin = deployment
        .origin()
        .ok_or_else(|| anyhow::anyhow!("Missing origin"))?;
    let imgref = origin
        .optional_string("origin", ostree_container::deploy::ORIGIN_CONTAINER)
        .context("Failed to load container image from origin")?
        .map(|v| ostree_container::OstreeImageReference::try_from(v.as_str()))
        .transpose()?;
    Ok((origin, imgref))
}

fn print_staged(deployment: &ostree::Deployment) -> Result<()> {
    let (_origin, imgref) = get_image_origin(deployment)?;
    let imgref = imgref.ok_or_else(|| {
        anyhow::anyhow!("Internal error: expected a container deployment to be staged")
    })?;
    println!("Queued for next boot: {imgref}");
    Ok(())
}

pub(crate) fn print_layer_status(prep: &ostree_container::store::PreparedImport) {
    let (stored, to_fetch, to_fetch_size) =
        prep.all_layers()
            .fold((0u32, 0u32, 0u64), |(stored, to_fetch, sz), v| {
                if v.commit.is_some() {
                    (stored + 1, to_fetch, sz)
                } else {
                    (stored, to_fetch + 1, sz + v.size())
                }
            });
    if to_fetch > 0 {
        let size = glib::format_size(to_fetch_size);
        println!("layers stored: {stored} needed: {to_fetch} ({size})")
    }
}

pub(crate) fn print_deprecated_warning(msg: &str) {
    eprintln!("warning: {msg}");
    std::thread::sleep(std::time::Duration::from_secs(3));
}

async fn upgrade(opts: UpgradeOpts) -> Result<()> {
    let cancellable = gio::Cancellable::NONE;
    let sysroot = &get_locked_sysroot().await?;
    let repo = &sysroot.repo().unwrap();
    let booted_deployment = &sysroot.require_booted_deployment()?;
    let osname = booted_deployment.osname().unwrap();
    let osname_v = Some(osname.as_str());
    let (origin, imgref) = get_image_origin(booted_deployment)?;
    let imgref =
        imgref.ok_or_else(|| anyhow::anyhow!("Booted deployment is not container image based"))?;
    let commit = booted_deployment.csum().unwrap();
    let state = ostree_container::store::query_image_commit(repo, &commit)?;
    let digest = state.manifest_digest.as_str();
    let fetched = pull(repo, &imgref, opts.quiet).await?;

    if fetched.merge_commit.as_str() == commit.as_str() {
        println!("Already queued: {digest}");
        return Ok(());
    }

    let merge_deployment = sysroot.merge_deployment(osname_v);

    let new_deployment = sysroot.stage_tree(
        osname_v,
        fetched.merge_commit.as_str(),
        Some(&origin),
        merge_deployment.as_ref(),
        &[],
        cancellable,
    )?;
    print_staged(&new_deployment)?;

    if let Some(path) = opts.touch_if_changed {
        std::fs::write(&path, "").with_context(|| format!("Writing {path}"))?;
    }

    Ok(())
}

async fn switch(opts: SwitchOpts) -> Result<()> {
    let cancellable = gio::Cancellable::NONE;
    let l = get_locked_sysroot().await?;
    let sysroot = l.deref();
    let booted_deployment = &sysroot.require_booted_deployment()?;
    let (origin, booted_image) = get_image_origin(booted_deployment)?;
    let booted_refspec = origin.optional_string("origin", "refspec")?;
    let osname = booted_deployment.osname().unwrap();
    let osname_v = Some(osname.as_str());
    let repo = &sysroot.repo().unwrap();

    let transport = ostree_container::Transport::try_from(opts.transport.as_str())?;
    let imgref = ostree_container::ImageReference {
        transport,
        name: opts.target.to_string(),
    };
    use ostree_container::SignatureSource;
    let sigverify = if opts.no_signature_verification {
        SignatureSource::ContainerPolicyAllowInsecure
    } else if let Some(remote) = opts.ostree_remote.as_ref() {
        SignatureSource::OstreeRemote(remote.to_string())
    } else {
        SignatureSource::ContainerPolicy
    };
    let target = ostree_container::OstreeImageReference { sigverify, imgref };

    let fetched = pull(repo, &target, opts.quiet).await?;
    let merge_deployment = sysroot.merge_deployment(osname_v);
    origin.set_string(
        "origin",
        ostree_container::deploy::ORIGIN_CONTAINER,
        target.to_string().as_str(),
    );

    if !opts.retain {
        // By default, we prune the previous ostree ref or container image
        if let Some(ostree_ref) = booted_refspec {
            repo.set_ref_immediate(None, &ostree_ref, None, cancellable)?;
            origin.remove_key("origin", "refspec")?;
        } else if let Some(booted_image) = booted_image.as_ref() {
            ostree_container::store::remove_image(repo, &booted_image.imgref)?;
            let _nlayers: u32 = ostree_container::store::gc_image_layers(repo)?;
        }
    }

    let new_deployment = sysroot.stage_tree(
        osname_v,
        fetched.merge_commit.as_str(),
        Some(&origin),
        merge_deployment.as_ref(),
        &[],
        cancellable,
    )?;
    print_staged(&new_deployment)?;

    Ok(())
}

fn serialize_image_reference<S>(
    txn: &Option<ostree_container::OstreeImageReference>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match txn {
        Some(v) => serializer.serialize_some(v.to_string().as_str()),
        None => serializer.serialize_none(),
    }
}

#[derive(serde::Serialize)]
struct DeploymentStatus {
    pinned: bool,
    booted: bool,
    staged: bool,
    #[serde(serialize_with = "serialize_image_reference")]
    image: Option<ostree_container::OstreeImageReference>,
    checksum: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    deploy_serial: Option<u32>,
}

async fn status(opts: StatusOpts) -> Result<()> {
    let l = get_locked_sysroot().await?;
    let sysroot = l.deref();
    let repo = &sysroot.repo().unwrap();
    let booted_deployment = &sysroot.require_booted_deployment()?;

    if opts.json {
        let deployments = sysroot
            .deployments()
            .into_iter()
            .filter(|deployment| !opts.booted || deployment.equal(booted_deployment))
            .map(|deployment| -> Result<DeploymentStatus> {
                let booted = deployment.equal(booted_deployment);
                let staged = deployment.is_staged();
                let pinned = deployment.is_pinned();
                let image = get_image_origin(&deployment)?.1;
                let checksum = deployment.csum().unwrap().to_string();
                let deploy_serial = (!staged).then(|| deployment.bootserial().try_into().unwrap());

                Ok(DeploymentStatus {
                    staged,
                    pinned,
                    booted,
                    image,
                    checksum,
                    deploy_serial,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let out = std::io::stdout();
        let mut out = out.lock();
        serde_json::to_writer(&mut out, &deployments).context("Writing to stdout")?;
        return Ok(());
    }

    for deployment in sysroot.deployments() {
        let booted = deployment.equal(booted_deployment);
        let booted_display = booted.then(|| "* ").unwrap_or(" ");

        let image = get_image_origin(&deployment)?.1;

        let commit = deployment.csum().unwrap();
        let serial = deployment.deployserial();
        if let Some(image) = image.as_ref() {
            println!("{booted_display} {image}");
            let state = ostree_container::store::query_image_commit(repo, &commit)?;
            println!("    Digest: {}", state.manifest_digest.as_str());
            let config = state.configuration.as_ref();
            let cconfig = config.and_then(|c| c.config().as_ref());
            let labels = cconfig.and_then(|c| c.labels().as_ref());
            if let Some(labels) = labels {
                if let Some(version) = labels.get("version") {
                    println!("    Version: {version}");
                }
            }
        } else {
            println!("{booted_display} {commit}.{serial}");
            println!("    (Non-container origin type)");
            println!();
        }
        println!("    Backend: ostree");
        if deployment.is_pinned() {
            println!("    Pinned: yes")
        }
        if booted {
            println!("    Booted: yes")
        } else if deployment.is_staged() {
            println!("    Staged: yes");
        }
        println!();
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
    ensure_self_unshared_mount_namespace().await?;
    let opt = Opt::parse_from(args);
    match opt {
        Opt::Upgrade(opts) => upgrade(opts).await,
        Opt::Switch(opts) => switch(opts).await,
        Opt::Status(opts) => status(opts).await,
        #[cfg(feature = "docgen")]
        Opt::Man(manopts) => crate::docgen::generate_manpages(&manopts.directory),
    }
}
