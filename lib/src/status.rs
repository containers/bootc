use anyhow::{Context, Result};
use ostree_container::OstreeImageReference;
use ostree_ext::container as ostree_container;

use crate::utils::{get_image_origin, ser_with_display};

/// Representation of a container image reference suitable for serialization to e.g. JSON.
#[derive(serde::Serialize)]
struct Image {
    #[serde(serialize_with = "ser_with_display")]
    verification: ostree_container::SignatureSource,
    #[serde(serialize_with = "ser_with_display")]
    transport: ostree_container::Transport,
    image: String,
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

/// Representation of a deployment suitable for serialization to e.g. JSON.
#[derive(serde::Serialize)]
struct DeploymentStatus {
    pinned: bool,
    booted: bool,
    staged: bool,
    image: Option<Image>,
    checksum: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    deploy_serial: Option<u32>,
}

/// Implementation of the `bootc status` CLI command.
pub(crate) async fn status(opts: super::cli::StatusOpts) -> Result<()> {
    let sysroot = super::cli::get_locked_sysroot().await?;
    let repo = &sysroot.repo().unwrap();
    let booted_deployment = &sysroot.require_booted_deployment()?;

    // If we're in JSON mode, then convert the ostree data into Rust-native
    // structures that can be serialized.
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
                    image: image.as_ref().map(Into::into),
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

    // We're not writing to JSON, so we directly iterate over the deployments.
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
