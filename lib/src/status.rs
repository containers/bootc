use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;

use anyhow::{Context, Result};
use fn_error_context::context;
use ostree::glib;
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::container_utils::ostree_booted;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::oci_spec;
use ostree_ext::ostree;

use crate::cli::OutputFormat;
use crate::spec::{BootEntry, BootOrder, Host, HostSpec, HostStatus, HostType};
use crate::spec::{ImageReference, ImageSignature};
use crate::store::{CachedImageStatus, ContainerImageStore, Storage};

impl From<ostree_container::SignatureSource> for ImageSignature {
    fn from(sig: ostree_container::SignatureSource) -> Self {
        use ostree_container::SignatureSource;
        match sig {
            SignatureSource::OstreeRemote(r) => Self::OstreeRemote(r),
            SignatureSource::ContainerPolicy => Self::ContainerPolicy,
            SignatureSource::ContainerPolicyAllowInsecure => Self::Insecure,
        }
    }
}

impl From<ImageSignature> for ostree_container::SignatureSource {
    fn from(sig: ImageSignature) -> Self {
        use ostree_container::SignatureSource;
        match sig {
            ImageSignature::OstreeRemote(r) => SignatureSource::OstreeRemote(r),
            ImageSignature::ContainerPolicy => Self::ContainerPolicy,
            ImageSignature::Insecure => Self::ContainerPolicyAllowInsecure,
        }
    }
}

/// Fixme lower serializability into ostree-ext
fn transport_to_string(transport: ostree_container::Transport) -> String {
    match transport {
        // Canonicalize to registry for our own use
        ostree_container::Transport::Registry => "registry".to_string(),
        o => {
            let mut s = o.to_string();
            s.truncate(s.rfind(':').unwrap());
            s
        }
    }
}

impl From<OstreeImageReference> for ImageReference {
    fn from(imgref: OstreeImageReference) -> Self {
        let signature = match imgref.sigverify {
            ostree_container::SignatureSource::ContainerPolicyAllowInsecure => None,
            v => Some(v.into()),
        };
        Self {
            signature,
            transport: transport_to_string(imgref.imgref.transport),
            image: imgref.imgref.name,
        }
    }
}

impl From<ImageReference> for OstreeImageReference {
    fn from(img: ImageReference) -> Self {
        let sigverify = match img.signature {
            Some(v) => v.into(),
            None => ostree_container::SignatureSource::ContainerPolicyAllowInsecure,
        };
        Self {
            sigverify,
            imgref: ostree_container::ImageReference {
                // SAFETY: We validated the schema in kube-rs
                transport: img.transport.as_str().try_into().unwrap(),
                name: img.image,
            },
        }
    }
}

/// Parse an ostree origin file (a keyfile) and extract the targeted
/// container image reference.
fn get_image_origin(origin: &glib::KeyFile) -> Result<Option<OstreeImageReference>> {
    origin
        .optional_string("origin", ostree_container::deploy::ORIGIN_CONTAINER)
        .context("Failed to load container image from origin")?
        .map(|v| ostree_container::OstreeImageReference::try_from(v.as_str()))
        .transpose()
}

pub(crate) struct Deployments {
    pub(crate) staged: Option<ostree::Deployment>,
    pub(crate) rollback: Option<ostree::Deployment>,
    #[allow(dead_code)]
    pub(crate) other: VecDeque<ostree::Deployment>,
}

pub(crate) fn try_deserialize_timestamp(t: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    match chrono::DateTime::parse_from_rfc3339(t).context("Parsing timestamp") {
        Ok(t) => Some(t.into()),
        Err(e) => {
            tracing::warn!("Invalid timestamp in image: {:#}", e);
            None
        }
    }
}

pub(crate) fn labels_of_config(
    config: &oci_spec::image::ImageConfiguration,
) -> Option<&std::collections::HashMap<String, String>> {
    config.config().as_ref().and_then(|c| c.labels().as_ref())
}

/// Given an OSTree deployment, parse out metadata into our spec.
#[context("Reading deployment metadata")]
fn boot_entry_from_deployment(
    sysroot: &Storage,
    deployment: &ostree::Deployment,
) -> Result<BootEntry> {
    let (
        store,
        CachedImageStatus {
            image,
            cached_update,
        },
        incompatible,
    ) = if let Some(origin) = deployment.origin().as_ref() {
        let incompatible = crate::utils::origin_has_rpmostree_stuff(origin);
        let (store, cached_imagestatus) = if incompatible {
            // If there are local changes, we can't represent it as a bootc compatible image.
            (None, CachedImageStatus::default())
        } else if let Some(image) = get_image_origin(origin)? {
            let store = deployment.store()?;
            let store = store.as_ref().unwrap_or(&sysroot.store);
            let spec = Some(store.spec());
            let status = store.imagestatus(sysroot, deployment, image)?;

            (spec, status)
        } else {
            // The deployment isn't using a container image
            (None, CachedImageStatus::default())
        };
        (store, cached_imagestatus, incompatible)
    } else {
        // The deployment has no origin at all (this generally shouldn't happen)
        (None, CachedImageStatus::default(), false)
    };

    let r = BootEntry {
        image,
        cached_update,
        incompatible,
        store,
        pinned: deployment.is_pinned(),
        ostree: Some(crate::spec::BootEntryOstree {
            checksum: deployment.csum().into(),
            // SAFETY: The deployserial is really unsigned
            deploy_serial: deployment.deployserial().try_into().unwrap(),
        }),
    };
    Ok(r)
}

impl BootEntry {
    /// Given a boot entry, find its underlying ostree container image
    pub(crate) fn query_image(
        &self,
        repo: &ostree::Repo,
    ) -> Result<Option<Box<ostree_container::store::LayeredImageState>>> {
        if self.image.is_none() {
            return Ok(None);
        }
        if let Some(checksum) = self.ostree.as_ref().map(|c| c.checksum.as_str()) {
            ostree_container::store::query_image_commit(repo, checksum).map(Some)
        } else {
            Ok(None)
        }
    }
}

/// A variant of [`get_status`] that requires a booted deployment.
pub(crate) fn get_status_require_booted(
    sysroot: &Storage,
) -> Result<(ostree::Deployment, Deployments, Host)> {
    let booted_deployment = sysroot.require_booted_deployment()?;
    let (deployments, host) = get_status(sysroot, Some(&booted_deployment))?;
    Ok((booted_deployment, deployments, host))
}

/// Gather the ostree deployment objects, but also extract metadata from them into
/// a more native Rust structure.
#[context("Computing status")]
pub(crate) fn get_status(
    sysroot: &Storage,
    booted_deployment: Option<&ostree::Deployment>,
) -> Result<(Deployments, Host)> {
    let stateroot = booted_deployment.as_ref().map(|d| d.osname());
    let (mut related_deployments, other_deployments) = sysroot
        .deployments()
        .into_iter()
        .partition::<VecDeque<_>, _>(|d| Some(d.osname()) == stateroot);
    let staged = related_deployments
        .iter()
        .position(|d| d.is_staged())
        .map(|i| related_deployments.remove(i).unwrap());
    tracing::debug!("Staged: {staged:?}");
    // Filter out the booted, the caller already found that
    if let Some(booted) = booted_deployment.as_ref() {
        related_deployments.retain(|f| !f.equal(booted));
    }
    let rollback = related_deployments.pop_front();
    let rollback_queued = match (booted_deployment.as_ref(), rollback.as_ref()) {
        (Some(booted), Some(rollback)) => rollback.index() < booted.index(),
        _ => false,
    };
    let boot_order = if rollback_queued {
        BootOrder::Rollback
    } else {
        BootOrder::Default
    };
    tracing::debug!("Rollback queued={rollback_queued:?}");
    let other = {
        related_deployments.extend(other_deployments);
        related_deployments
    };
    let deployments = Deployments {
        staged,
        rollback,
        other,
    };

    let staged = deployments
        .staged
        .as_ref()
        .map(|d| boot_entry_from_deployment(sysroot, d))
        .transpose()
        .context("Staged deployment")?;
    let booted = booted_deployment
        .as_ref()
        .map(|d| boot_entry_from_deployment(sysroot, d))
        .transpose()
        .context("Booted deployment")?;
    let rollback = deployments
        .rollback
        .as_ref()
        .map(|d| boot_entry_from_deployment(sysroot, d))
        .transpose()
        .context("Rollback deployment")?;
    let spec = staged
        .as_ref()
        .or(booted.as_ref())
        .and_then(|entry| entry.image.as_ref())
        .map(|img| HostSpec {
            image: Some(img.image.clone()),
            boot_order,
        })
        .unwrap_or_default();

    let ty = if booted
        .as_ref()
        .map(|b| b.image.is_some())
        .unwrap_or_default()
    {
        // We're only of type BootcHost if we booted via container image
        Some(HostType::BootcHost)
    } else {
        None
    };

    let mut host = Host::new(spec);
    host.status = HostStatus {
        staged,
        booted,
        rollback,
        rollback_queued,
        ty,
    };
    Ok((deployments, host))
}

/// Implementation of the `bootc status` CLI command.
#[context("Status")]
pub(crate) async fn status(opts: super::cli::StatusOpts) -> Result<()> {
    match opts.format_version.unwrap_or_default() {
        // For historical reasons, both 0 and 1 mean "v1".
        0 | 1 => {}
        o => anyhow::bail!("Unsupported format version: {o}"),
    };
    let host = if !ostree_booted()? {
        Default::default()
    } else {
        let sysroot = super::cli::get_storage().await?;
        let booted_deployment = sysroot.booted_deployment();
        let (_deployments, host) = get_status(&sysroot, booted_deployment.as_ref())?;
        host
    };

    // If we're in JSON mode, then convert the ostree data into Rust-native
    // structures that can be serialized.
    // Filter to just the serializable status structures.
    let out = std::io::stdout();
    let mut out = out.lock();
    let legacy_opt = if opts.json {
        OutputFormat::Json
    } else if std::io::stdout().is_terminal() {
        OutputFormat::HumanReadable
    } else {
        OutputFormat::Yaml
    };
    let format = opts.format.unwrap_or(legacy_opt);
    match format {
        OutputFormat::Json => serde_json::to_writer(&mut out, &host).map_err(anyhow::Error::new),
        OutputFormat::Yaml => serde_yaml::to_writer(&mut out, &host).map_err(anyhow::Error::new),
        OutputFormat::HumanReadable => human_readable_output(&mut out, &host),
    }
    .context("Writing to stdout")?;

    Ok(())
}

#[derive(Debug)]
enum Slot {
    Staged,
    Booted,
    Rollback,
}

impl std::fmt::Display for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Slot::Staged => "staged",
            Slot::Booted => "booted",
            Slot::Rollback => "rollback",
        };
        f.write_str(s)
    }
}

/// Output a row title, prefixed by spaces
fn write_row_name(mut out: impl Write, s: &str, prefix_len: usize) -> Result<()> {
    let n = prefix_len.saturating_sub(s.chars().count());
    let mut spaces = std::io::repeat(b' ').take(n as u64);
    std::io::copy(&mut spaces, &mut out)?;
    write!(out, "{s}: ")?;
    Ok(())
}

/// Write the data for a container image based status.
fn human_render_imagestatus(
    mut out: impl Write,
    slot: Slot,
    image: &crate::spec::ImageStatus,
) -> Result<()> {
    let transport = &image.image.transport;
    let imagename = &image.image.image;
    // Registry is the default, so don't show that
    let imageref = if transport == "registry" {
        Cow::Borrowed(imagename)
    } else {
        // But for non-registry we include the transport
        Cow::Owned(format!("{transport}:{imagename}"))
    };
    let prefix = match slot {
        Slot::Staged => "  Staged image".into(),
        Slot::Booted => format!("{} Booted image", crate::glyph::Glyph::BlackCircle),
        Slot::Rollback => "  Rollback image".into(),
    };
    let prefix_len = prefix.chars().count();
    writeln!(out, "{prefix}: {imageref}")?;

    write_row_name(&mut out, "Digest", prefix_len)?;
    let digest = &image.image_digest;
    writeln!(out, "{digest}")?;

    // Format the timestamp without nanoseconds since those are just irrelevant noise for human
    // consumption - that time scale should basically never matter for container builds.
    let timestamp = image
        .timestamp
        .as_ref()
        // This format is the same as RFC3339, just without nanos.
        .map(|t| t.to_utc().format("%Y-%m-%dT%H:%M:%SZ"));
    // If we have a version, combine with timestamp
    if let Some(version) = image.version.as_deref() {
        write_row_name(&mut out, "Version", prefix_len)?;
        if let Some(timestamp) = timestamp {
            writeln!(out, "{version} ({timestamp})")?;
        } else {
            writeln!(out, "{version}")?;
        }
    } else if let Some(timestamp) = timestamp {
        // Otherwise just output timestamp
        write_row_name(&mut out, "Timestamp", prefix_len)?;
        writeln!(out, "{timestamp}")?;
    }

    Ok(())
}

fn human_render_ostree(mut out: impl Write, slot: Slot, ostree_commit: &str) -> Result<()> {
    // TODO consider rendering more ostree stuff here like rpm-ostree status does
    let prefix = match slot {
        Slot::Staged => "  Staged ostree".into(),
        Slot::Booted => format!("{} Booted ostree", crate::glyph::Glyph::BlackCircle),
        Slot::Rollback => "  Rollback ostree".into(),
    };
    let prefix_len = prefix.len();
    writeln!(out, "{prefix}")?;
    write_row_name(&mut out, "Commit", prefix_len)?;
    writeln!(out, "{ostree_commit}")?;
    Ok(())
}

fn human_readable_output_booted(mut out: impl Write, host: &Host) -> Result<()> {
    let mut first = true;
    for (slot_name, status) in [
        (Slot::Staged, &host.status.staged),
        (Slot::Booted, &host.status.booted),
        (Slot::Rollback, &host.status.rollback),
    ] {
        if let Some(host_status) = status {
            if first {
                first = false;
            } else {
                writeln!(out)?;
            }
            if let Some(image) = &host_status.image {
                human_render_imagestatus(&mut out, slot_name, image)?;
            } else if let Some(ostree) = host_status.ostree.as_ref() {
                human_render_ostree(&mut out, slot_name, &ostree.checksum)?;
            } else {
                writeln!(out, "Current {slot_name} state is unknown")?;
            }
        }
    }
    Ok(())
}

/// Implementation of rendering our host structure in a "human readable" way.
fn human_readable_output(mut out: impl Write, host: &Host) -> Result<()> {
    if host.status.booted.is_some() {
        human_readable_output_booted(out, host)?;
    } else {
        writeln!(out, "System is not deployed via bootc.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human_status_from_spec_fixture(spec_fixture: &str) -> Result<String> {
        let host: Host = serde_yaml::from_str(spec_fixture).unwrap();
        let mut w = Vec::new();
        human_readable_output(&mut w, &host).unwrap();
        let w = String::from_utf8(w).unwrap();
        Ok(w)
    }

    #[test]
    fn test_human_readable_base_spec() {
        // Tests Staged and Booted, null Rollback
        let w = human_status_from_spec_fixture(include_str!("fixtures/spec-staged-booted.yaml"))
            .expect("No spec found");
        let expected = indoc::indoc! { r"
            Staged image: quay.io/example/someimage:latest
                  Digest: sha256:16dc2b6256b4ff0d2ec18d2dbfb06d117904010c8cf9732cdb022818cf7a7566
                 Version: nightly (2023-10-14T19:22:15Z)
        
          ● Booted image: quay.io/example/someimage:latest
                  Digest: sha256:736b359467c9437c1ac915acaae952aad854e07eb4a16a94999a48af08c83c34
                 Version: nightly (2023-09-30T19:22:16Z)
        "};
        similar_asserts::assert_eq!(w, expected);
    }

    #[test]
    fn test_human_readable_rfe_spec() {
        // Basic rhel for edge bootc install with nothing
        let w = human_status_from_spec_fixture(include_str!(
            "fixtures/spec-rfe-ostree-deployment.yaml"
        ))
        .expect("No spec found");
        let expected = indoc::indoc! { r"
            Staged ostree
                   Commit: 1c24260fdd1be20f72a4a97a75c582834ee3431fbb0fa8e4f482bb219d633a45
          
          ● Booted ostree
                     Commit: f9fa3a553ceaaaf30cf85bfe7eed46a822f7b8fd7e14c1e3389cbc3f6d27f791
        "};
        similar_asserts::assert_eq!(w, expected);
    }

    #[test]
    fn test_human_readable_staged_spec() {
        // staged image, no boot/rollback
        let w = human_status_from_spec_fixture(include_str!("fixtures/spec-ostree-to-bootc.yaml"))
            .expect("No spec found");
        let expected = indoc::indoc! { r"
            Staged image: quay.io/centos-bootc/centos-bootc:stream9
                  Digest: sha256:47e5ed613a970b6574bfa954ab25bb6e85656552899aa518b5961d9645102b38
                 Version: stream9.20240807.0
          
          ● Booted ostree
                     Commit: f9fa3a553ceaaaf30cf85bfe7eed46a822f7b8fd7e14c1e3389cbc3f6d27f791
        "};
        similar_asserts::assert_eq!(w, expected);
    }

    #[test]
    fn test_human_readable_booted_spec() {
        // booted image, no staged/rollback
        let w = human_status_from_spec_fixture(include_str!("fixtures/spec-only-booted.yaml"))
            .expect("No spec found");
        let expected = indoc::indoc! { r"
          ● Booted image: quay.io/centos-bootc/centos-bootc:stream9
                  Digest: sha256:47e5ed613a970b6574bfa954ab25bb6e85656552899aa518b5961d9645102b38
                 Version: stream9.20240807.0
        "};
        similar_asserts::assert_eq!(w, expected);
    }

    #[test]
    fn test_human_readable_staged_rollback_spec() {
        // staged/rollback image, no booted
        let w = human_status_from_spec_fixture(include_str!("fixtures/spec-staged-rollback.yaml"))
            .expect("No spec found");
        let expected = "System is not deployed via bootc.\n";
        similar_asserts::assert_eq!(w, expected);
    }

    #[test]
    fn test_via_oci() {
        let w = human_status_from_spec_fixture(include_str!("fixtures/spec-via-local-oci.yaml"))
            .unwrap();
        let expected = indoc::indoc! { r"
          ● Booted image: oci:/var/mnt/osupdate
                  Digest: sha256:47e5ed613a970b6574bfa954ab25bb6e85656552899aa518b5961d9645102b38
                 Version: stream9.20240807.0
        "};
        similar_asserts::assert_eq!(w, expected);
    }

    #[test]
    fn test_convert_signatures() {
        use std::str::FromStr;
        let ir_unverified = &OstreeImageReference::from_str(
            "ostree-unverified-registry:quay.io/someexample/foo:latest",
        )
        .unwrap();
        let ir_ostree = &OstreeImageReference::from_str(
            "ostree-remote-registry:fedora:quay.io/fedora/fedora-coreos:stable",
        )
        .unwrap();

        let ir = ImageReference::from(ir_unverified.clone());
        assert_eq!(ir.image, "quay.io/someexample/foo:latest");
        assert_eq!(ir.signature, None);

        let ir = ImageReference::from(ir_ostree.clone());
        assert_eq!(ir.image, "quay.io/fedora/fedora-coreos:stable");
        assert_eq!(
            ir.signature,
            Some(ImageSignature::OstreeRemote("fedora".into()))
        );
    }
}
