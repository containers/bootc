//! # Write deployments merging image with configmap
//!
//! Create a merged filesystem tree with the image and mounted configmaps.

use std::collections::HashSet;
use std::io::{BufRead, Write};

use anyhow::Ok;
use anyhow::{anyhow, Context, Result};
use cap_std::fs::{Dir, MetadataExt};
use cap_std_ext::cap_std;
use cap_std_ext::dirext::CapStdExtDirExt;
use fn_error_context::context;
use ostree::{gio, glib};
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::container::store::{ImportProgress, PrepareResult};
use ostree_ext::oci_spec::image::{Descriptor, Digest};
use ostree_ext::ostree::Deployment;
use ostree_ext::ostree::{self, Sysroot};
use ostree_ext::sysroot::SysrootLock;

use crate::spec::ImageReference;
use crate::spec::{BootOrder, HostSpec};
use crate::status::labels_of_config;
use crate::store::Storage;

// TODO use https://github.com/ostreedev/ostree-rs-ext/pull/493/commits/afc1837ff383681b947de30c0cefc70080a4f87a
const BASE_IMAGE_PREFIX: &str = "ostree/container/baseimage/bootc";

/// Set on an ostree commit if this is a derived commit
const BOOTC_DERIVED_KEY: &str = "bootc.derived";

/// Variant of HostSpec but required to be filled out
pub(crate) struct RequiredHostSpec<'a> {
    pub(crate) image: &'a ImageReference,
}

/// State of a locally fetched image
pub(crate) struct ImageState {
    pub(crate) manifest_digest: Digest,
    pub(crate) version: Option<String>,
    pub(crate) ostree_commit: String,
}

impl<'a> RequiredHostSpec<'a> {
    /// Given a (borrowed) host specification, "unwrap" its internal
    /// options, giving a spec that is required to have a base container image.
    pub(crate) fn from_spec(spec: &'a HostSpec) -> Result<Self> {
        let image = spec
            .image
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing image in specification"))?;
        Ok(Self { image })
    }
}

impl From<ostree_container::store::LayeredImageState> for ImageState {
    fn from(value: ostree_container::store::LayeredImageState) -> Self {
        let version = value.version().map(|v| v.to_owned());
        let ostree_commit = value.get_commit().to_owned();
        Self {
            manifest_digest: value.manifest_digest,
            version,
            ostree_commit,
        }
    }
}

impl ImageState {
    /// Fetch the manifest corresponding to this image.  May not be available in all backends.
    pub(crate) fn get_manifest(
        &self,
        repo: &ostree::Repo,
    ) -> Result<Option<ostree_ext::oci_spec::image::ImageManifest>> {
        ostree_container::store::query_image_commit(repo, &self.ostree_commit)
            .map(|v| Some(v.manifest))
    }
}

/// Wrapper for pulling a container image, wiring up status output.
pub(crate) async fn new_importer(
    repo: &ostree::Repo,
    imgref: &ostree_container::OstreeImageReference,
) -> Result<ostree_container::store::ImageImporter> {
    let config = Default::default();
    let mut imp = ostree_container::store::ImageImporter::new(repo, imgref, config).await?;
    imp.require_bootable();
    Ok(imp)
}

pub(crate) fn check_bootc_label(config: &ostree_ext::oci_spec::image::ImageConfiguration) {
    if let Some(label) =
        labels_of_config(config).and_then(|labels| labels.get(crate::metadata::BOOTC_COMPAT_LABEL))
    {
        match label.as_str() {
            crate::metadata::COMPAT_LABEL_V1 => {}
            o => crate::journal::journal_print(
                libsystemd::logging::Priority::Warning,
                &format!(
                    "notice: Unknown {} value {}",
                    crate::metadata::BOOTC_COMPAT_LABEL,
                    o
                ),
            ),
        }
    } else {
        crate::journal::journal_print(
            libsystemd::logging::Priority::Warning,
            &format!(
                "notice: Image is missing label: {}",
                crate::metadata::BOOTC_COMPAT_LABEL
            ),
        )
    }
}

fn descriptor_of_progress(p: &ImportProgress) -> &Descriptor {
    match p {
        ImportProgress::OstreeChunkStarted(l) => l,
        ImportProgress::OstreeChunkCompleted(l) => l,
        ImportProgress::DerivedLayerStarted(l) => l,
        ImportProgress::DerivedLayerCompleted(l) => l,
    }
}

fn prefix_of_progress(p: &ImportProgress) -> &'static str {
    match p {
        ImportProgress::OstreeChunkStarted(_) | ImportProgress::OstreeChunkCompleted(_) => {
            "ostree chunk"
        }
        ImportProgress::DerivedLayerStarted(_) | ImportProgress::DerivedLayerCompleted(_) => {
            "layer"
        }
    }
}

/// Write container fetch progress to standard output.
async fn handle_layer_progress_print(
    mut layers: tokio::sync::mpsc::Receiver<ostree_container::store::ImportProgress>,
    mut layer_bytes: tokio::sync::watch::Receiver<Option<ostree_container::store::LayerProgress>>,
    n_layers_to_fetch: usize,
) {
    let start = std::time::Instant::now();
    let mut total_read = 0u64;
    let bar = indicatif::MultiProgress::new();
    let layers_bar = bar.add(indicatif::ProgressBar::new(
        n_layers_to_fetch.try_into().unwrap(),
    ));
    let byte_bar = bar.add(indicatif::ProgressBar::new(0));
    // let byte_bar = indicatif::ProgressBar::new(0);
    // byte_bar.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    layers_bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("{prefix} {bar} {pos}/{len} {wide_msg}")
            .unwrap(),
    );
    layers_bar.set_prefix("Fetching layers");
    layers_bar.set_message("");
    byte_bar.set_prefix("Fetching");
    byte_bar.set_style(
        indicatif::ProgressStyle::default_bar()
                .template(
                    " └ {prefix} {bar} {binary_bytes}/{binary_total_bytes} ({binary_bytes_per_sec}) {wide_msg}",
                )
                .unwrap()
        );
    loop {
        tokio::select! {
            // Always handle layer changes first.
            biased;
            layer = layers.recv() => {
                if let Some(l) = layer {
                    let layer = descriptor_of_progress(&l);
                    let layer_size = layer.size();
                    if l.is_starting() {
                        byte_bar.reset_elapsed();
                        byte_bar.reset_eta();
                        byte_bar.set_length(layer_size);
                        let layer_type = prefix_of_progress(&l);
                        let short_digest = &layer.digest().digest()[0..21];
                        byte_bar.set_message(format!("{layer_type} {short_digest}"));
                    } else {
                        byte_bar.set_position(layer_size);
                        layers_bar.inc(1);
                        total_read = total_read.saturating_add(layer_size);
                    }
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
                    byte_bar.set_position(bytes.fetched);
                }
            }
        }
    }
    byte_bar.finish_and_clear();
    layers_bar.finish_and_clear();
    if let Err(e) = bar.clear() {
        tracing::warn!("clearing bar: {e}");
    }
    let end = std::time::Instant::now();
    let elapsed = end.duration_since(start);
    let persec = total_read as f64 / elapsed.as_secs_f64();
    let persec = indicatif::HumanBytes(persec as u64);
    println!(
        "Fetched layers: {} in {} ({}/s)",
        indicatif::HumanBytes(total_read),
        indicatif::HumanDuration(elapsed),
        persec,
    );
}

/// Wrapper for pulling a container image, wiring up status output.
#[context("Pulling")]
pub(crate) async fn pull(
    repo: &ostree::Repo,
    imgref: &ImageReference,
    target_imgref: Option<&OstreeImageReference>,
    quiet: bool,
) -> Result<Box<ImageState>> {
    let ostree_imgref = &OstreeImageReference::from(imgref.clone());
    let mut imp = new_importer(repo, ostree_imgref).await?;
    if let Some(target) = target_imgref {
        imp.set_target(target);
    }
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => {
            println!("No changes in {imgref:#} => {}", c.manifest_digest);
            return Ok(Box::new((*c).into()));
        }
        PrepareResult::Ready(p) => p,
    };
    check_bootc_label(&prep.config);
    if let Some(warning) = prep.deprecated_warning() {
        ostree_ext::cli::print_deprecated_warning(warning).await;
    }
    ostree_ext::cli::print_layer_status(&prep);
    let layers_to_fetch = prep.layers_to_fetch().collect::<Result<Vec<_>>>()?;
    let n_layers_to_fetch = layers_to_fetch.len();
    let printer = (!quiet).then(|| {
        let layer_progress = imp.request_progress();
        let layer_byte_progress = imp.request_layer_progress();
        tokio::task::spawn(async move {
            handle_layer_progress_print(layer_progress, layer_byte_progress, n_layers_to_fetch)
                .await
        })
    });
    let import = imp.import(prep).await;
    if let Some(printer) = printer {
        let _ = printer.await;
    }
    let import = import?;
    let wrote_imgref = target_imgref.as_ref().unwrap_or(&ostree_imgref);
    if let Some(msg) =
        ostree_container::store::image_filtered_content_warning(repo, &wrote_imgref.imgref)
            .context("Image content warning")?
    {
        crate::journal::journal_print(libsystemd::logging::Priority::Notice, &msg);
    }
    Ok(Box::new((*import).into()))
}

/// Gather all bound images in all deployments, then prune the image store,
/// using the gathered images as the roots (that will not be GC'd).
pub(crate) async fn prune_container_store(sysroot: &Storage) -> Result<()> {
    let deployments = sysroot.deployments();
    let mut all_bound_images = Vec::new();
    for deployment in deployments {
        let bound = crate::boundimage::query_bound_images_for_deployment(sysroot, &deployment)?;
        all_bound_images.extend(bound.into_iter());
    }
    // Convert to a hashset of just the image names
    let image_names = HashSet::from_iter(all_bound_images.iter().map(|img| img.image.as_str()));
    let pruned = sysroot
        .get_ensure_imgstore()?
        .prune_except_roots(&image_names)
        .await?;
    tracing::debug!("Pruned images: {}", pruned.len());
    Ok(())
}

pub(crate) async fn wipe_ostree(sysroot: Sysroot) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        sysroot
            .write_deployments(&[], gio::Cancellable::NONE)
            .context("removing deployments")
    })
    .await??;

    Ok(())
}

pub(crate) async fn cleanup(sysroot: &Storage) -> Result<()> {
    let bound_prune = prune_container_store(sysroot);

    // We create clones (just atomic reference bumps) here to move to the thread.
    let repo = sysroot.repo();
    let sysroot = sysroot.sysroot.clone();
    let repo_prune =
        ostree_ext::tokio_util::spawn_blocking_cancellable_flatten(move |cancellable| {
            let locked_sysroot = &SysrootLock::from_assumed_locked(&sysroot);
            let cancellable = Some(cancellable);
            let repo = &repo;
            let txn = repo.auto_transaction(cancellable)?;
            let repo = txn.repo();

            // Regenerate our base references.  First, we delete the ones that exist
            for ref_entry in repo
                .list_refs_ext(
                    Some(BASE_IMAGE_PREFIX),
                    ostree::RepoListRefsExtFlags::NONE,
                    cancellable,
                )
                .context("Listing refs")?
                .keys()
            {
                repo.transaction_set_refspec(ref_entry, None);
            }

            // Then, for each deployment which is derived (e.g. has configmaps) we synthesize
            // a base ref to ensure that it's not GC'd.
            for (i, deployment) in sysroot.deployments().into_iter().enumerate() {
                let commit = deployment.csum();
                if let Some(base) = get_base_commit(repo, &commit)? {
                    repo.transaction_set_refspec(&format!("{BASE_IMAGE_PREFIX}/{i}"), Some(&base));
                }
            }

            let pruned =
                ostree_container::deploy::prune(locked_sysroot).context("Pruning images")?;
            if !pruned.is_empty() {
                let size = glib::format_size(pruned.objsize);
                println!(
                    "Pruned images: {} (layers: {}, objsize: {})",
                    pruned.n_images, pruned.n_layers, size
                );
            } else {
                tracing::debug!("Nothing to prune");
            }

            Ok(())
        });

    // We run these in parallel mostly because we can.
    tokio::try_join!(repo_prune, bound_prune)?;
    Ok(())
}

/// If commit is a bootc-derived commit (e.g. has configmaps), return its base.
#[context("Finding base commit")]
pub(crate) fn get_base_commit(repo: &ostree::Repo, commit: &str) -> Result<Option<String>> {
    let commitv = repo.load_commit(commit)?.0;
    let commitmeta = commitv.child_value(0);
    let commitmeta = &glib::VariantDict::new(Some(&commitmeta));
    let r = commitmeta.lookup::<String>(BOOTC_DERIVED_KEY)?;
    Ok(r)
}

#[context("Writing deployment")]
async fn deploy(
    sysroot: &Storage,
    merge_deployment: Option<&Deployment>,
    stateroot: &str,
    image: &ImageState,
    origin: &glib::KeyFile,
) -> Result<Deployment> {
    let mut opts = ostree::SysrootDeployTreeOpts::default();
    // Compute the kernel argument overrides. In practice today this API is always expecting
    // a merge deployment. The kargs code also always looks at the booted root (which
    // is a distinct minor issue, but not super important as right now the install path
    // doesn't use this API).
    let override_kargs = if let Some(deployment) = merge_deployment {
        Some(crate::kargs::get_kargs(sysroot, &deployment, image)?)
    } else {
        None
    };
    // Because the C API expects a Vec<&str>, we need to generate a new Vec<>
    // that borrows.
    let override_kargs = override_kargs
        .as_deref()
        .map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    if let Some(kargs) = override_kargs.as_deref() {
        opts.override_kernel_argv = Some(&kargs);
    }
    // Copy to move into thread
    let cancellable = gio::Cancellable::NONE;
    return sysroot
        .stage_tree_with_options(
            Some(stateroot),
            image.ostree_commit.as_str(),
            Some(origin),
            merge_deployment,
            &opts,
            cancellable,
        )
        .map_err(Into::into);
}

#[context("Generating origin")]
fn origin_from_imageref(imgref: &ImageReference) -> Result<glib::KeyFile> {
    let origin = glib::KeyFile::new();
    let imgref = OstreeImageReference::from(imgref.clone());
    origin.set_string(
        "origin",
        ostree_container::deploy::ORIGIN_CONTAINER,
        imgref.to_string().as_str(),
    );
    Ok(origin)
}

/// Stage (queue deployment of) a fetched container image.
#[context("Staging")]
pub(crate) async fn stage(
    sysroot: &Storage,
    merge_stateroot: &str,
    stateroot: &str,
    image: &ImageState,
    spec: &RequiredHostSpec<'_>,
) -> Result<()> {
    let merge_deployment = sysroot.merge_deployment(Some(merge_stateroot));
    let origin = origin_from_imageref(spec.image)?;
    let deployment = crate::deploy::deploy(
        sysroot,
        merge_deployment.as_ref(),
        stateroot,
        image,
        &origin,
    )
    .await?;

    crate::boundimage::pull_bound_images(sysroot, &deployment).await?;

    crate::deploy::cleanup(sysroot).await?;
    println!("Queued for next boot: {:#}", spec.image);
    if let Some(version) = image.version.as_deref() {
        println!("  Version: {version}");
    }
    println!("  Digest: {}", image.manifest_digest);

    Ok(())
}

/// Implementation of rollback functionality
pub(crate) async fn rollback(sysroot: &Storage) -> Result<()> {
    const ROLLBACK_JOURNAL_ID: &str = "26f3b1eb24464d12aa5e7b544a6b5468";
    let repo = &sysroot.repo();
    let (booted_deployment, deployments, host) = crate::status::get_status_require_booted(sysroot)?;

    let new_spec = {
        let mut new_spec = host.spec.clone();
        new_spec.boot_order = new_spec.boot_order.swap();
        new_spec
    };

    // Just to be sure
    host.spec.verify_transition(&new_spec)?;

    let reverting = new_spec.boot_order == BootOrder::Default;
    if reverting {
        println!("notice: Reverting queued rollback state");
    }
    let rollback_status = host
        .status
        .rollback
        .ok_or_else(|| anyhow!("No rollback available"))?;
    let rollback_image = rollback_status
        .query_image(repo)?
        .ok_or_else(|| anyhow!("Rollback is not container image based"))?;
    let msg = format!("Rolling back to image: {}", rollback_image.manifest_digest);
    libsystemd::logging::journal_send(
        libsystemd::logging::Priority::Info,
        &msg,
        [
            ("MESSAGE_ID", ROLLBACK_JOURNAL_ID),
            (
                "BOOTC_MANIFEST_DIGEST",
                &rollback_image.manifest_digest.to_string(),
            ),
        ]
        .into_iter(),
    )?;
    // SAFETY: If there's a rollback status, then there's a deployment
    let rollback_deployment = deployments.rollback.expect("rollback deployment");
    let new_deployments = if reverting {
        [booted_deployment, rollback_deployment]
    } else {
        [rollback_deployment, booted_deployment]
    };
    let new_deployments = new_deployments
        .into_iter()
        .chain(deployments.other)
        .collect::<Vec<_>>();
    tracing::debug!("Writing new deployments: {new_deployments:?}");
    sysroot.write_deployments(&new_deployments, gio::Cancellable::NONE)?;
    if reverting {
        println!("Next boot: current deployment");
    } else {
        println!("Next boot: rollback deployment");
    }
    Ok(())
}

fn find_newest_deployment_name(deploysdir: &Dir) -> Result<String> {
    let mut dirs = Vec::new();
    for ent in deploysdir.entries()? {
        let ent = ent?;
        if !ent.file_type()?.is_dir() {
            continue;
        }
        let name = ent.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        dirs.push((name.to_owned(), ent.metadata()?.mtime()));
    }
    dirs.sort_unstable_by(|a, b| a.1.cmp(&b.1));
    if let Some((name, _ts)) = dirs.pop() {
        Ok(name)
    } else {
        anyhow::bail!("No deployment directory found")
    }
}

// Implementation of `bootc switch --in-place`
pub(crate) fn switch_origin_inplace(root: &Dir, imgref: &ImageReference) -> Result<String> {
    // First, just create the new origin file
    let origin = origin_from_imageref(imgref)?;
    let serialized_origin = origin.to_data();

    // Now, we can't rely on being officially booted (e.g. with the `ostree=` karg)
    // in a scenario like running in the anaconda %post.
    // Eventually, we should support a setup here where ostree-prepare-root
    // can officially be run to "enter" an ostree root in a supportable way.
    // Anyways for now, the brutal hack is to just scrape through the deployments
    // and find the newest one, which we will mutate.  If there's more than one,
    // ultimately the calling tooling should be fixed to set things up correctly.

    let mut ostree_deploys = root.open_dir("sysroot/ostree/deploy")?.entries()?;
    let deploydir = loop {
        if let Some(ent) = ostree_deploys.next() {
            let ent = ent?;
            if !ent.file_type()?.is_dir() {
                continue;
            }
            tracing::debug!("Checking {:?}", ent.file_name());
            let child_dir = ent
                .open_dir()
                .with_context(|| format!("Opening dir {:?}", ent.file_name()))?;
            if let Some(d) = child_dir.open_dir_optional("deploy")? {
                break d;
            }
        } else {
            anyhow::bail!("Failed to find a deployment");
        }
    };
    let newest_deployment = find_newest_deployment_name(&deploydir)?;
    let origin_path = format!("{newest_deployment}.origin");
    if !deploydir.try_exists(&origin_path)? {
        tracing::warn!("No extant origin for {newest_deployment}");
    }
    deploydir
        .atomic_write(&origin_path, serialized_origin.as_bytes())
        .context("Writing origin")?;
    Ok(newest_deployment)
}

#[test]
fn test_switch_inplace() -> Result<()> {
    use cap_std::fs::DirBuilderExt;

    let td = cap_std_ext::cap_tempfile::TempDir::new(cap_std::ambient_authority())?;
    let mut builder = cap_std::fs::DirBuilder::new();
    let builder = builder.recursive(true).mode(0o755);
    let deploydir = "sysroot/ostree/deploy/default/deploy";
    let target_deployment = "af36eb0086bb55ac601600478c6168f834288013d60f8870b7851f44bf86c3c5.0";
    td.ensure_dir_with(
        format!("sysroot/ostree/deploy/default/deploy/{target_deployment}"),
        builder,
    )?;
    let deploydir = &td.open_dir(deploydir)?;
    let orig_imgref = ImageReference {
        image: "quay.io/exampleos/original:sometag".into(),
        transport: "registry".into(),
        signature: None,
    };
    {
        let origin = origin_from_imageref(&orig_imgref)?;
        deploydir.atomic_write(
            format!("{target_deployment}.origin"),
            origin.to_data().as_bytes(),
        )?;
    }

    let target_imgref = ImageReference {
        image: "quay.io/someother/otherimage:latest".into(),
        transport: "registry".into(),
        signature: None,
    };

    let replaced = switch_origin_inplace(&td, &target_imgref).unwrap();
    assert_eq!(replaced, target_deployment);
    Ok(())
}

/// A workaround for https://github.com/ostreedev/ostree/issues/3193
/// as generated by anaconda.
#[context("Updating /etc/fstab for anaconda+composefs")]
pub(crate) fn fixup_etc_fstab(root: &Dir) -> Result<()> {
    let fstab_path = "etc/fstab";
    // Read the old file
    let fd = root
        .open(fstab_path)
        .with_context(|| format!("Opening {fstab_path}"))
        .map(std::io::BufReader::new)?;

    // Helper function to possibly change a line from /etc/fstab.
    // Returns Ok(true) if we made a change (and we wrote the modified line)
    // otherwise returns Ok(false) and the caller should write the original line.
    fn edit_fstab_line(line: &str, mut w: impl Write) -> Result<bool> {
        if line.starts_with('#') {
            return Ok(false);
        }
        let parts = line.split_ascii_whitespace().collect::<Vec<_>>();

        let path_idx = 1;
        let options_idx = 3;
        let (&path, &options) = match (parts.get(path_idx), parts.get(options_idx)) {
            (None, _) => {
                tracing::debug!("No path in entry: {line}");
                return Ok(false);
            }
            (_, None) => {
                tracing::debug!("No options in entry: {line}");
                return Ok(false);
            }
            (Some(p), Some(o)) => (p, o),
        };
        // If this is not the root, we're not matching on it
        if path != "/" {
            return Ok(false);
        }
        // If options already contains `ro`, nothing to do
        if options.split(',').any(|s| s == "ro") {
            return Ok(false);
        }

        writeln!(w, "# {}", crate::generator::BOOTC_EDITED_STAMP)?;

        // SAFETY: we unpacked the options before.
        // This adds `ro` to the option list
        assert!(!options.is_empty()); // Split wouldn't have turned this up if it was empty
        let options = format!("{options},ro");
        for (i, part) in parts.into_iter().enumerate() {
            // TODO: would obviously be nicer to preserve whitespace...but...eh.
            if i > 0 {
                write!(w, " ")?;
            }
            if i == options_idx {
                write!(w, "{options}")?;
            } else {
                write!(w, "{part}")?
            }
        }
        // And add the trailing newline
        writeln!(w)?;
        Ok(true)
    }

    // Read the input, and atomically write a modified version
    root.atomic_replace_with(fstab_path, move |mut w| {
        for line in fd.lines() {
            let line = line?;
            if !edit_fstab_line(&line, &mut w)? {
                writeln!(w, "{line}")?;
            }
        }
        Ok(())
    })
    .context("Replacing /etc/fstab")?;

    println!("Updated /etc/fstab to add `ro` for `/`");
    Ok(())
}

#[test]
fn test_fixup_etc_fstab_default() -> Result<()> {
    let tempdir = cap_std_ext::cap_tempfile::tempdir(cap_std::ambient_authority())?;
    let default = "UUID=f7436547-20ac-43cb-aa2f-eac9632183f6 /boot auto ro 0 0\n";
    tempdir.create_dir_all("etc")?;
    tempdir.atomic_write("etc/fstab", default)?;
    fixup_etc_fstab(&tempdir).unwrap();
    assert_eq!(tempdir.read_to_string("etc/fstab")?, default);
    Ok(())
}

#[test]
fn test_fixup_etc_fstab_multi() -> Result<()> {
    let tempdir = cap_std_ext::cap_tempfile::tempdir(cap_std::ambient_authority())?;
    let default = "UUID=f7436547-20ac-43cb-aa2f-eac9632183f6 /boot auto ro 0 0\n\
UUID=6907-17CA          /boot/efi               vfat    umask=0077,shortname=winnt 0 2\n";
    tempdir.create_dir_all("etc")?;
    tempdir.atomic_write("etc/fstab", default)?;
    fixup_etc_fstab(&tempdir).unwrap();
    assert_eq!(tempdir.read_to_string("etc/fstab")?, default);
    Ok(())
}

#[test]
fn test_fixup_etc_fstab_ro() -> Result<()> {
    let tempdir = cap_std_ext::cap_tempfile::tempdir(cap_std::ambient_authority())?;
    let default = "UUID=f7436547-20ac-43cb-aa2f-eac9632183f6 /boot auto ro 0 0\n\
UUID=1eef9f42-40e3-4bd8-ae20-e9f2325f8b52 /                     xfs   ro 0 0\n\
UUID=6907-17CA          /boot/efi               vfat    umask=0077,shortname=winnt 0 2\n";
    tempdir.create_dir_all("etc")?;
    tempdir.atomic_write("etc/fstab", default)?;
    fixup_etc_fstab(&tempdir).unwrap();
    assert_eq!(tempdir.read_to_string("etc/fstab")?, default);
    Ok(())
}

#[test]
fn test_fixup_etc_fstab_rw() -> Result<()> {
    let tempdir = cap_std_ext::cap_tempfile::tempdir(cap_std::ambient_authority())?;
    // This case uses `defaults`
    let default = "UUID=f7436547-20ac-43cb-aa2f-eac9632183f6 /boot auto ro 0 0\n\
UUID=1eef9f42-40e3-4bd8-ae20-e9f2325f8b52 /                     xfs   defaults 0 0\n\
UUID=6907-17CA          /boot/efi               vfat    umask=0077,shortname=winnt 0 2\n";
    let modified = "UUID=f7436547-20ac-43cb-aa2f-eac9632183f6 /boot auto ro 0 0\n\
# Updated by bootc-fstab-edit.service\n\
UUID=1eef9f42-40e3-4bd8-ae20-e9f2325f8b52 / xfs defaults,ro 0 0\n\
UUID=6907-17CA          /boot/efi               vfat    umask=0077,shortname=winnt 0 2\n";
    tempdir.create_dir_all("etc")?;
    tempdir.atomic_write("etc/fstab", default)?;
    fixup_etc_fstab(&tempdir).unwrap();
    assert_eq!(tempdir.read_to_string("etc/fstab")?, modified);
    Ok(())
}
