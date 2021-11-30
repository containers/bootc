//! APIs for creating container images from OSTree commits

use super::ociwriter::OciWriter;
use super::*;
use crate::tar as ostree_tar;
use anyhow::Context;
use fn_error_context::context;
use gio::glib;
use ostree::gio;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
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
    writer: &mut OciWriter,
    compression: Option<flate2::Compression>,
) -> Result<ociwriter::Layer> {
    let commit = repo.resolve_rev(rev, false)?.unwrap();
    let mut w = writer.create_raw_layer(compression)?;
    ostree_tar::export_commit(repo, commit.as_str(), &mut w)?;
    w.complete()
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
    let ocidir = &openat::Dir::open(ocidir_path)?;
    let mut writer = ociwriter::OciWriter::new(ocidir)?;

    let commit = repo.resolve_rev(rev, false)?.unwrap();
    let commit = commit.as_str();
    let (commit_v, _) = repo.load_commit(commit)?;
    let commit_meta = &commit_v.child_value(0);
    let commit_meta = glib::VariantDict::new(Some(commit_meta));

    if let Some(version) =
        commit_meta.lookup_value("version", Some(glib::VariantTy::new("s").unwrap()))
    {
        let version = version.str().unwrap();
        writer.add_config_annotation("version", version);
        writer.add_manifest_annotation("ostree.version", version);
    }

    writer.add_config_annotation(OSTREE_COMMIT_LABEL, commit);
    writer.add_manifest_annotation(OSTREE_COMMIT_LABEL, commit);

    for (k, v) in config.labels.iter().map(|k| k.iter()).flatten() {
        writer.add_config_annotation(k, v);
    }
    if let Some(cmd) = config.cmd.as_ref() {
        let cmd: Vec<_> = cmd.iter().map(|s| s.as_str()).collect();
        writer.set_cmd(&cmd);
    }
    let compression = if opts.compress {
        flate2::Compression::default()
    } else {
        flate2::Compression::none()
    };

    let rootfs_blob = export_ostree_ref(repo, commit, &mut writer, Some(compression))?;
    let mut annos = HashMap::new();
    annos.insert(BLOB_OSTREE_ANNOTATION.to_string(), "true".to_string());
    writer.push_layer_annotated(rootfs_blob, Some(annos));
    writer.complete()?;

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
        let digestfile = if skopeo::skopeo_has_features(skopeo::SkopeoFeatures::COPY_DIGESTFILE)? {
            Some(tempdir.path().join("digestfile"))
        } else {
            None
        };

        let src = build_oci(repo, ostree_ref, Path::new(tempdest), config, opts)?;

        let mut cmd = skopeo::new_cmd();
        tracing::event!(Level::DEBUG, "Copying {} to {}", src, dest);
        cmd.stdout(std::process::Stdio::null()).arg("copy");
        if let Some(ref digestfile) = digestfile {
            cmd.arg("--digestfile");
            cmd.arg(digestfile);
        }
        cmd.args(&[src.to_string(), dest.to_string()]);
        let proc = super::skopeo::spawn(cmd)?;
        let output = proc.wait_with_output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("skopeo failed: {}\n", stderr));
        }
        digestfile
            .map(|p| -> Result<String> { Ok(std::fs::read_to_string(p)?.trim().to_string()) })
            .transpose()?
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
