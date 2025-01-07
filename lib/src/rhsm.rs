//! Integration with Red Hat Subscription Manager

use anyhow::{Context, Result};
use cap_std::fs::Dir;
use cap_std_ext::{cap_std, dirext::CapStdExtDirExt};
use fn_error_context::context;
use serde::Serialize;

const FACTS_PATH: &str = "etc/rhsm/facts/bootc.facts";

#[derive(Serialize, PartialEq, Eq, Debug, Default)]
struct RhsmFacts {
    #[serde(rename = "bootc.booted.image")]
    booted_image: String,
    #[serde(rename = "bootc.booted.version")]
    booted_version: String,
    #[serde(rename = "bootc.booted.digest")]
    booted_digest: String,
    #[serde(rename = "bootc.staged.image")]
    staged_image: String,
    #[serde(rename = "bootc.staged.version")]
    staged_version: String,
    #[serde(rename = "bootc.staged.digest")]
    staged_digest: String,
    #[serde(rename = "bootc.rollback.image")]
    rollback_image: String,
    #[serde(rename = "bootc.rollback.version")]
    rollback_version: String,
    #[serde(rename = "bootc.rollback.digest")]
    rollback_digest: String,
    #[serde(rename = "bootc.available.image")]
    available_image: String,
    #[serde(rename = "bootc.available.version")]
    available_version: String,
    #[serde(rename = "bootc.available.digest")]
    available_digest: String,
}

/// Return the image reference, version and digest as owned strings.
/// A missing version is serialized as the empty string.
fn status_to_strings(imagestatus: &crate::spec::ImageStatus) -> (String, String, String) {
    let image = imagestatus.image.image.clone();
    let version = imagestatus.version.as_ref().cloned().unwrap_or_default();
    let digest = imagestatus.image_digest.clone();
    (image, version, digest)
}

impl From<crate::spec::HostStatus> for RhsmFacts {
    fn from(hoststatus: crate::spec::HostStatus) -> Self {
        let (booted_image, booted_version, booted_digest) = hoststatus
            .booted
            .as_ref()
            .and_then(|boot_entry| boot_entry.image.as_ref().map(status_to_strings))
            .unwrap_or_default();

        let (staged_image, staged_version, staged_digest) = hoststatus
            .staged
            .as_ref()
            .and_then(|boot_entry| boot_entry.image.as_ref().map(status_to_strings))
            .unwrap_or_default();

        let (rollback_image, rollback_version, rollback_digest) = hoststatus
            .rollback
            .as_ref()
            .and_then(|boot_entry| boot_entry.image.as_ref().map(status_to_strings))
            .unwrap_or_default();

        let (available_image, available_version, available_digest) = hoststatus
            .booted
            .as_ref()
            .and_then(|boot_entry| boot_entry.cached_update.as_ref().map(status_to_strings))
            .unwrap_or_default();

        Self {
            booted_image,
            booted_version,
            booted_digest,
            staged_image,
            staged_version,
            staged_digest,
            rollback_image,
            rollback_version,
            rollback_digest,
            available_image,
            available_version,
            available_digest,
        }
    }
}

/// Publish facts for subscription-manager consumption
#[context("Publishing facts")]
pub(crate) async fn publish_facts(root: &Dir) -> Result<()> {
    let sysroot = super::cli::get_storage().await?;
    let booted_deployment = sysroot.booted_deployment();
    let (_deployments, host) = crate::status::get_status(&sysroot, booted_deployment.as_ref())?;

    let facts = RhsmFacts::from(host.status);
    root.atomic_replace_with(FACTS_PATH, |w| {
        serde_json::to_writer_pretty(w, &facts)?;
        anyhow::Ok(())
    })
    .with_context(|| format!("Writing {FACTS_PATH}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::spec::Host;

    #[test]
    fn test_rhsm_facts_from_host() {
        let host: Host = serde_yaml::from_str(include_str!("fixtures/spec-staged-booted.yaml"))
            .expect("No spec found");
        let facts = RhsmFacts::from(host.status);

        assert_eq!(
            facts,
            RhsmFacts {
                booted_image: "quay.io/example/someimage:latest".into(),
                booted_version: "nightly".into(),
                booted_digest:
                    "sha256:736b359467c9437c1ac915acaae952aad854e07eb4a16a94999a48af08c83c34".into(),
                staged_image: "quay.io/example/someimage:latest".into(),
                staged_version: "nightly".into(),
                staged_digest:
                    "sha256:16dc2b6256b4ff0d2ec18d2dbfb06d117904010c8cf9732cdb022818cf7a7566".into(),
                ..Default::default()
            }
        );
    }
}
