//! # Write deployments merging image with configmap
//!
//! Create a merged filesystem tree with the image and mounted configmaps.

use anyhow::Ok;
use anyhow::{Context, Result};

use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use cap_std_ext::dirext::CapStdExtDirExt;
use fn_error_context::context;
use ostree::{gio, glib};
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::container::store::PrepareResult;
use ostree_ext::ostree;
use ostree_ext::ostree::Deployment;
use ostree_ext::sysroot::SysrootLock;
use rustix::fs::MetadataExt;

use crate::spec::HostSpec;
use crate::spec::ImageReference;

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
    pub(crate) manifest_digest: String,
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

/// Wrapper for pulling a container image, wiring up status output.
#[context("Pulling")]
pub(crate) async fn pull(
    sysroot: &SysrootLock,
    imgref: &ImageReference,
    quiet: bool,
) -> Result<Box<ImageState>> {
    let repo = &sysroot.repo();
    let imgref = &OstreeImageReference::from(imgref.clone());
    let mut imp = new_importer(repo, imgref).await?;
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => {
            println!("No changes in {} => {}", imgref, c.manifest_digest);
            return Ok(Box::new((*c).into()));
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
    Ok(Box::new((*import).into()))
}

pub(crate) async fn cleanup(sysroot: &SysrootLock) -> Result<()> {
    // We create clones (just atomic reference bumps) here to move to the thread.
    let repo = sysroot.repo();
    let sysroot = sysroot.sysroot.clone();
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

        let pruned = ostree_container::deploy::prune(locked_sysroot).context("Pruning images")?;
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
    })
    .await
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
    sysroot: &SysrootLock,
    merge_deployment: Option<&Deployment>,
    stateroot: &str,
    image: &ImageState,
    origin: &glib::KeyFile,
) -> Result<()> {
    let stateroot = Some(stateroot);
    // Copy to move into thread
    let cancellable = gio::Cancellable::NONE;
    let _new_deployment = sysroot.stage_tree_with_options(
        stateroot,
        image.ostree_commit.as_str(),
        Some(origin),
        merge_deployment,
        &Default::default(),
        cancellable,
    )?;
    Ok(())
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
    sysroot: &SysrootLock,
    stateroot: &str,
    image: &ImageState,
    spec: &RequiredHostSpec<'_>,
) -> Result<()> {
    let merge_deployment = sysroot.merge_deployment(Some(stateroot));
    let origin = origin_from_imageref(spec.image)?;
    crate::deploy::deploy(
        sysroot,
        merge_deployment.as_ref(),
        stateroot,
        image,
        &origin,
    )
    .await?;
    crate::deploy::cleanup(sysroot).await?;
    println!("Queued for next boot: {}", spec.image);
    if let Some(version) = image.version.as_deref() {
        println!("  Version: {version}");
    }
    println!("  Digest: {}", image.manifest_digest);

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
        let name = if let Some(name) = name.to_str() {
            name
        } else {
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
    return Ok(newest_deployment);
}

#[test]
fn test_switch_inplace() -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

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
