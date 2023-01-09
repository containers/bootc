use std::borrow::Cow;

use anyhow::{Context, Result};
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;
use ostree_ext::ostree;
use ostree_ext::sysroot::SysrootLock;

use crate::utils::{get_image_origin, ser_with_display};

/// Representation of a container image reference suitable for serialization to e.g. JSON.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct Image {
    #[serde(serialize_with = "ser_with_display")]
    pub(crate) verification: ostree_container::SignatureSource,
    #[serde(serialize_with = "ser_with_display")]
    pub(crate) transport: ostree_container::Transport,
    pub(crate) image: String,
}

impl From<&OstreeImageReference> for Image {
    fn from(imgref: &OstreeImageReference) -> Self {
        Self {
            verification: imgref.sigverify.clone(),
            transport: imgref.imgref.transport,
            image: imgref.imgref.name.clone(),
        }
    }
}

impl From<Image> for OstreeImageReference {
    fn from(img: Image) -> OstreeImageReference {
        OstreeImageReference {
            sigverify: img.verification,
            imgref: ostree_container::ImageReference {
                transport: img.transport,
                name: img.image,
            },
        }
    }
}

/// Representation of a deployment suitable for serialization to e.g. JSON.
#[derive(serde::Serialize)]
pub(crate) struct DeploymentStatus {
    pub(crate) pinned: bool,
    pub(crate) booted: bool,
    pub(crate) staged: bool,
    pub(crate) supported: bool,
    pub(crate) image: Option<Image>,
    pub(crate) checksum: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deploy_serial: Option<u32>,
}

impl DeploymentStatus {
    /// Gather metadata from an ostree deployment into a Rust structure
    pub(crate) fn from_deployment(deployment: &ostree::Deployment, booted: bool) -> Result<Self> {
        let staged = deployment.is_staged();
        let pinned = deployment.is_pinned();
        let image = get_image_origin(deployment)?.1;
        let checksum = deployment.csum().unwrap().to_string();
        let deploy_serial = (!staged).then(|| deployment.bootserial().try_into().unwrap());
        let supported = deployment
            .origin()
            .map(|o| !crate::utils::origin_has_rpmostree_stuff(&o))
            .unwrap_or_default();

        Ok(DeploymentStatus {
            staged,
            pinned,
            booted,
            supported,
            image: image.as_ref().map(Into::into),
            checksum,
            deploy_serial,
        })
    }
}

/// Gather the ostree deployment objects, but also extract metadata from them into
/// a more native Rust structure.
fn get_deployments(
    sysroot: &SysrootLock,
    booted_deployment: Option<&ostree::Deployment>,
    booted_only: bool,
) -> Result<Vec<(ostree::Deployment, DeploymentStatus)>> {
    let deployment_is_booted = |d: &ostree::Deployment| -> bool {
        booted_deployment.as_ref().map_or(false, |b| d.equal(b))
    };
    sysroot
        .deployments()
        .into_iter()
        .filter(|deployment| !booted_only || deployment_is_booted(deployment))
        .map(|deployment| -> Result<_> {
            let booted = deployment_is_booted(&deployment);
            let status = DeploymentStatus::from_deployment(&deployment, booted)?;
            Ok((deployment, status))
        })
        .collect()
}

/// Implementation of the `bootc status` CLI command.
pub(crate) async fn status(opts: super::cli::StatusOpts) -> Result<()> {
    let sysroot = super::cli::get_locked_sysroot().await?;
    let repo = &sysroot.repo().unwrap();
    let booted_deployment = sysroot.booted_deployment();

    let deployments = get_deployments(&sysroot, booted_deployment.as_ref(), opts.booted)?;
    // If we're in JSON mode, then convert the ostree data into Rust-native
    // structures that can be serialized.
    if opts.json {
        // Filter to just the serializable status structures.
        let deployments = deployments.into_iter().map(|e| e.1).collect::<Vec<_>>();
        let out = std::io::stdout();
        let mut out = out.lock();
        serde_json::to_writer(&mut out, &deployments).context("Writing to stdout")?;
        return Ok(());
    }

    // We're not writing to JSON; iterate over and print.
    for (deployment, info) in deployments {
        let booted_display = info.booted.then(|| "* ").unwrap_or(" ");
        let image: Option<OstreeImageReference> = info.image.as_ref().map(|i| i.clone().into());

        let commit = info.checksum;
        if let Some(image) = image.as_ref() {
            println!("{booted_display} {image}");
            if !info.supported {
                println!("    Origin contains rpm-ostree machine-local changes");
            } else {
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
            }
        } else {
            let deployinfo = if let Some(serial) = info.deploy_serial {
                Cow::Owned(format!("{commit}.{serial}"))
            } else {
                Cow::Borrowed(&commit)
            };
            println!("{booted_display} {deployinfo}");
            println!("    (Non-container origin type)");
            println!();
        }
        println!("    Backend: ostree");
        if deployment.is_pinned() {
            println!("    Pinned: yes")
        }
        if info.booted {
            println!("    Booted: yes")
        } else if deployment.is_staged() {
            println!("    Staged: yes");
        }
        println!();
    }

    Ok(())
}
