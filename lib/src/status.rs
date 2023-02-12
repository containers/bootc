use std::collections::VecDeque;

use crate::spec::{BootEntry, Host, HostSpec, HostStatus, ImageStatus};
use crate::spec::{ImageReference, ImageSignature};
use anyhow::{Context, Result};
use ostree::glib;
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;
use ostree_ext::sysroot::SysrootLock;

const OBJECT_NAME: &str = "host";

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
        Self {
            signature: imgref.sigverify.into(),
            transport: transport_to_string(imgref.imgref.transport),
            image: imgref.imgref.name,
        }
    }
}

impl From<ImageReference> for OstreeImageReference {
    fn from(img: ImageReference) -> Self {
        Self {
            sigverify: img.signature.into(),
            imgref: ostree_container::ImageReference {
                /// SAFETY: We validated the schema in kube-rs
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

fn boot_entry_from_deployment(
    sysroot: &SysrootLock,
    deployment: &ostree::Deployment,
) -> Result<BootEntry> {
    let repo = &sysroot.repo();
    let mut incompatible = false;
    let configmaps = crate::config::configs_for_deployment(sysroot, deployment)?
        .into_iter()
        .map(|v| v.name)
        .collect();
    let mut image = None;
    if let Some(origin) = deployment.origin().as_ref() {
        if let Some(imgref) = get_image_origin(origin)? {
            let imgref = ImageReference::from(imgref);
            let csum = deployment.csum();
            incompatible = crate::utils::origin_has_rpmostree_stuff(origin);
            let imgstate = ostree_container::store::query_image_commit(repo, &csum)?;
            image = Some(ImageStatus {
                image: imgref,
                image_digest: imgstate.manifest_digest,
            });
        }
    }

    let r = BootEntry {
        image,
        configmaps,
        incompatible,
        pinned: deployment.is_pinned(),
        ostree: Some(crate::spec::BootEntryOstree {
            checksum: deployment.csum().into(),
            // SAFETY: The deployserial is really unsigned
            deploy_serial: deployment.deployserial().try_into().unwrap(),
        }),
    };
    Ok(r)
}

/// Gather the ostree deployment objects, but also extract metadata from them into
/// a more native Rust structure.
pub(crate) fn get_status(
    sysroot: &SysrootLock,
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
    // Filter out the booted, the caller already found that
    if let Some(booted) = booted_deployment.as_ref() {
        related_deployments.retain(|f| !f.equal(booted));
    }
    let rollback = related_deployments.pop_front();
    let other = {
        related_deployments.extend(other_deployments);
        related_deployments
    };
    let deployments = Deployments {
        staged,
        rollback,
        other,
    };

    let is_container = ostree_ext::container_utils::is_ostree_container()?;

    let staged = deployments
        .staged
        .as_ref()
        .map(|d| boot_entry_from_deployment(sysroot, d))
        .transpose()?;
    let booted = booted_deployment
        .as_ref()
        .map(|d| boot_entry_from_deployment(sysroot, d))
        .transpose()?;
    let rollback = deployments
        .rollback
        .as_ref()
        .map(|d| boot_entry_from_deployment(sysroot, d))
        .transpose()?;
    let spec = staged
        .as_ref()
        .or(booted.as_ref())
        .and_then(|entry| {
            if let Some(img) = entry.image.as_ref() {
                Some(HostSpec {
                    image: Some(img.image.clone()),
                    configmap_sources: entry.configmaps.iter().cloned().collect(),
                })
            } else {
                None
            }
        })
        .unwrap_or_default();
    let mut host = Host::new(OBJECT_NAME, spec);
    host.status = Some(HostStatus {
        staged,
        booted,
        rollback,
        is_container,
    });
    Ok((deployments, host))
}

/// Implementation of the `bootc status` CLI command.
pub(crate) async fn status(opts: super::cli::StatusOpts) -> Result<()> {
    let host = if ostree_ext::container_utils::is_ostree_container()? {
        let status = HostStatus {
            is_container: true,
            ..Default::default()
        };
        let mut r = Host::new(OBJECT_NAME, HostSpec::default());
        r.status = Some(status);
        r
    } else {
        let sysroot = super::cli::get_locked_sysroot().await?;
        let booted_deployment = sysroot.booted_deployment();
        let (_deployments, host) = get_status(&sysroot, booted_deployment.as_ref())?;
        host
    };

    // If we're in JSON mode, then convert the ostree data into Rust-native
    // structures that can be serialized.
    // Filter to just the serializable status structures.
    let out = std::io::stdout();
    let mut out = out.lock();
    if opts.json {
        serde_json::to_writer(&mut out, &host).context("Writing to stdout")?;
    } else {
        serde_yaml::to_writer(&mut out, &host).context("Writing to stdout")?;
    }

    Ok(())
}
