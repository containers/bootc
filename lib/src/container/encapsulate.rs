//! APIs for creating container images from OSTree commits

use super::ocidir::OciDir;
use super::{ocidir, OstreeImageReference, Transport};
use super::{ImageReference, SignatureSource, OSTREE_COMMIT_LABEL};
use crate::container::skopeo;
use crate::tar as ostree_tar;
use anyhow::{anyhow, Context, Result};
use fn_error_context::context;
use gio::glib;
use oci_spec::image as oci_image;
use ostree::gio;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::rc::Rc;
use tracing::{instrument, Level};

/// Annotation injected into the layer to say that this is an ostree commit.
/// However, because this gets lost when converted to D2S2 https://docs.docker.com/registry/spec/manifest-v2-2/
/// schema, it's not actually useful today.  But, we keep it
/// out of principle.
const BLOB_OSTREE_ANNOTATION: &str = "ostree.encapsulated";
/// Configuration for the generated container.
#[derive(Debug, Default)]
pub struct Config {
    /// Additional labels.
    pub labels: Option<BTreeMap<String, String>>,
    /// The equivalent of a `Dockerfile`'s `CMD` instruction.
    pub cmd: Option<Vec<String>>,
}

/// Write an ostree commit to an OCI blob
#[context("Writing ostree root to blob")]
fn export_ostree_ref(
    repo: &ostree::Repo,
    rev: &str,
    writer: &mut OciDir,
    compression: Option<flate2::Compression>,
) -> Result<ocidir::Layer> {
    let commit = repo.require_rev(rev)?;
    let mut w = writer.create_raw_layer(compression)?;
    ostree_tar::export_commit(repo, commit.as_str(), &mut w, None)?;
    w.complete()
}

fn commit_meta_to_labels<'a>(
    meta: &glib::VariantDict,
    keys: impl IntoIterator<Item = &'a str>,
    labels: &mut HashMap<String, String>,
) -> Result<()> {
    for k in keys {
        let v = meta
            .lookup::<String>(k)
            .context("Expected string for commit metadata value")?
            .ok_or_else(|| anyhow!("Could not find commit metadata key: {}", k))?;
        labels.insert(k.to_string(), v);
    }
    // Copy standard metadata keys `ostree.bootable` and `ostree.linux`.
    // Bootable is an odd one out in being a boolean.
    if let Some(v) = meta.lookup::<bool>(*ostree::METADATA_KEY_BOOTABLE)? {
        labels.insert(ostree::METADATA_KEY_BOOTABLE.to_string(), v.to_string());
    }
    // Handle any other string-typed values here.
    for k in &[&ostree::METADATA_KEY_LINUX] {
        if let Some(v) = meta.lookup::<String>(k)? {
            labels.insert(k.to_string(), v);
        }
    }
    Ok(())
}

/// Generate an OCI image from a given ostree root
#[context("Building oci")]
fn build_oci(
    repo: &ostree::Repo,
    rev: &str,
    ocidir_path: &Path,
    config: &Config,
    opts: ExportOpts,
) -> Result<ImageReference> {
    // Explicitly error if the target exists
    std::fs::create_dir(ocidir_path).context("Creating OCI dir")?;
    let ocidir = Rc::new(openat::Dir::open(ocidir_path)?);
    let mut writer = ocidir::OciDir::create(ocidir)?;

    let commit = repo.require_rev(rev)?;
    let commit = commit.as_str();
    let (commit_v, _) = repo.load_commit(commit)?;
    let commit_subject = commit_v.child_value(3);
    let commit_subject = commit_subject.str().ok_or_else(|| {
        anyhow::anyhow!(
            "Corrupted commit {}; expecting string value for subject",
            commit
        )
    })?;
    let commit_meta = &commit_v.child_value(0);
    let commit_meta = glib::VariantDict::new(Some(commit_meta));

    let mut ctrcfg = oci_image::Config::default();
    let mut imgcfg = oci_image::ImageConfiguration::default();
    let labels = ctrcfg.labels_mut().get_or_insert_with(Default::default);

    commit_meta_to_labels(
        &commit_meta,
        opts.copy_meta_keys.iter().map(|k| k.as_str()),
        labels,
    )?;

    let mut manifest = ocidir::new_empty_manifest().build().unwrap();

    if let Some(version) =
        commit_meta.lookup_value("version", Some(glib::VariantTy::new("s").unwrap()))
    {
        let version = version.str().unwrap();
        labels.insert("version".into(), version.into());
    }

    labels.insert(OSTREE_COMMIT_LABEL.into(), commit.into());

    for (k, v) in config.labels.iter().map(|k| k.iter()).flatten() {
        labels.insert(k.into(), v.into());
    }
    // Lookup the cmd embedded in commit metadata
    let cmd = commit_meta.lookup::<Vec<String>>(ostree::COMMIT_META_CONTAINER_CMD)?;
    // But support it being overridden by CLI options
    let cmd = config.cmd.as_ref().or_else(|| cmd.as_ref());
    if let Some(cmd) = cmd {
        ctrcfg.set_cmd(Some(cmd.clone()));
    }

    imgcfg.set_config(Some(ctrcfg));

    let compression = if opts.compress {
        flate2::Compression::default()
    } else {
        flate2::Compression::none()
    };

    let rootfs_blob = export_ostree_ref(repo, commit, &mut writer, Some(compression))?;
    let description = if commit_subject.is_empty() {
        Cow::Owned(format!("ostree export of commit {}", commit))
    } else {
        Cow::Borrowed(commit_subject)
    };
    let mut annos = HashMap::new();
    annos.insert(BLOB_OSTREE_ANNOTATION.to_string(), "true".to_string());
    writer.push_layer_annotated(
        &mut manifest,
        &mut imgcfg,
        rootfs_blob,
        Some(annos),
        &description,
    );
    let ctrcfg = writer.write_config(imgcfg)?;
    manifest.set_config(ctrcfg);
    writer.write_manifest(manifest, oci_image::Platform::default())?;

    Ok(ImageReference {
        transport: Transport::OciDir,
        name: ocidir_path.to_str().unwrap().to_string(),
    })
}

/// Helper for `build()` that avoids generics
#[instrument(skip(repo))]
async fn build_impl(
    repo: &ostree::Repo,
    ostree_ref: &str,
    config: &Config,
    opts: Option<ExportOpts>,
    dest: &ImageReference,
) -> Result<String> {
    let mut opts = opts.unwrap_or_default();
    if dest.transport == Transport::ContainerStorage {
        opts.compress = false;
    }
    let digest = if dest.transport == Transport::OciDir {
        let _copied: ImageReference = build_oci(
            repo,
            ostree_ref,
            Path::new(dest.name.as_str()),
            config,
            opts,
        )?;
        None
    } else {
        let tempdir = tempfile::tempdir_in("/var/tmp")?;
        let tempdest = tempdir.path().join("d");
        let tempdest = tempdest.to_str().unwrap();
        let digestfile = tempdir.path().join("digestfile");

        let src = build_oci(repo, ostree_ref, Path::new(tempdest), config, opts)?;

        let mut cmd = skopeo::new_cmd();
        tracing::event!(Level::DEBUG, "Copying {} to {}", src, dest);
        cmd.stdout(std::process::Stdio::null()).arg("copy");
        cmd.arg("--digestfile");
        cmd.arg(&digestfile);
        cmd.args(&[src.to_string(), dest.to_string()]);
        let proc = super::skopeo::spawn(cmd)?;
        let output = proc.wait_with_output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("skopeo failed: {}\n", stderr));
        }
        Some(std::fs::read_to_string(digestfile)?.trim().to_string())
    };
    if let Some(digest) = digest {
        Ok(digest)
    } else {
        // If `skopeo copy` doesn't have `--digestfile` yet, then fall back
        // to running an inspect cycle.
        let imgref = OstreeImageReference {
            sigverify: SignatureSource::ContainerPolicyAllowInsecure,
            imgref: dest.to_owned(),
        };
        let (_, digest) = super::unencapsulate::fetch_manifest(&imgref).await?;
        Ok(digest)
    }
}

/// Options controlling commit export into OCI
#[derive(Debug, Default)]
pub struct ExportOpts {
    /// If true, perform gzip compression of the tar layers.
    pub compress: bool,
    /// A set of commit metadata keys to copy as image labels.
    pub copy_meta_keys: Vec<String>,
}

/// Given an OSTree repository and ref, generate a container image.
///
/// The returned `ImageReference` will contain a digested (e.g. `@sha256:`) version of the destination.
pub async fn encapsulate<S: AsRef<str>>(
    repo: &ostree::Repo,
    ostree_ref: S,
    config: &Config,
    opts: Option<ExportOpts>,
    dest: &ImageReference,
) -> Result<String> {
    build_impl(repo, ostree_ref.as_ref(), config, opts, dest).await
}
