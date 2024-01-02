//! The definition for host system state.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::k8sapitypes;

const API_VERSION: &str = "org.containers.bootc/v1alpha1";
const KIND: &str = "BootcHost";
/// The default object name we use; there's only one.
pub(crate) const OBJECT_NAME: &str = "host";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// The core host definition
pub struct Host {
    /// Metadata
    #[serde(flatten)]
    pub resource: k8sapitypes::Resource,
    /// The spec
    #[serde(default)]
    pub spec: HostSpec,
    /// The status
    #[serde(default)]
    pub status: HostStatus,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// The host specification
pub struct HostSpec {
    /// The host image
    pub image: Option<ImageReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
/// An image signature
#[serde(rename_all = "camelCase")]
pub enum ImageSignature {
    /// Fetches will use the named ostree remote for signature verification of the ostree commit.
    OstreeRemote(String),
    /// Fetches will defer to the `containers-policy.json`, but we make a best effort to reject `default: insecureAcceptAnything` policy.
    ContainerPolicy,
    /// No signature verification will be performed
    Insecure,
}

/// A container image reference with attached transport and signature verification
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageReference {
    /// The container image reference
    pub image: String,
    /// The container image transport
    pub transport: String,
    /// Signature verification type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<ImageSignature>,
}

/// The status of the booted image
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageStatus {
    /// The currently booted image
    pub image: ImageReference,
    /// The version string, if any
    pub version: Option<String>,
    /// The build timestamp, if any
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
    /// The digest of the fetched image (e.g. sha256:a0...);
    pub image_digest: String,
}

/// A bootable entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BootEntryOstree {
    /// The ostree commit checksum
    pub checksum: String,
    /// The deployment serial
    pub deploy_serial: u32,
}

/// A bootable entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BootEntry {
    /// The image reference
    pub image: Option<ImageStatus>,
    /// Whether this boot entry is not compatible (has origin changes bootc does not understand)
    pub incompatible: bool,
    /// Whether this entry will be subject to garbage collection
    pub pinned: bool,
    /// If this boot entry is ostree based, the corresponding state
    pub ostree: Option<BootEntryOstree>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
/// The detected type of running system.  Note that this is not exhaustive
/// and new variants may be added in the future.
pub enum HostType {
    /// The current system is deployed in a bootc compatible way.
    BootcHost,
}

/// The status of the host system
#[derive(Debug, Clone, Serialize, Default, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    /// The staged image for the next boot
    pub staged: Option<BootEntry>,
    /// The booted image; this will be unset if the host is not bootc compatible.
    pub booted: Option<BootEntry>,
    /// The previously booted image
    pub rollback: Option<BootEntry>,

    /// The detected type of system
    #[serde(rename = "type")]
    pub ty: Option<HostType>,
}

impl Host {
    /// Create a new host
    pub fn new(spec: HostSpec) -> Self {
        let metadata = k8sapitypes::ObjectMeta {
            name: Some(OBJECT_NAME.to_owned()),
            ..Default::default()
        };
        Self {
            resource: k8sapitypes::Resource {
                api_version: API_VERSION.to_owned(),
                kind: KIND.to_owned(),
                metadata,
            },
            spec,
            status: Default::default(),
        }
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_spec_v0() {
        const SPEC_FIXTURE: &str = include_str!("fixtures/spec.yaml");
        let host: Host = serde_yaml::from_str(SPEC_FIXTURE).unwrap();
        assert_eq!(
            host.spec.image.as_ref().unwrap().image.as_str(),
            "quay.io/example/someimage:latest"
        );
    }

    #[test]
    fn test_parse_spec_v1() {
        const SPEC_FIXTURE: &str = include_str!("fixtures/spec-v1.yaml");
        let host: Host = serde_yaml::from_str(SPEC_FIXTURE).unwrap();
        assert_eq!(
            host.spec.image.as_ref().unwrap().image.as_str(),
            "quay.io/otherexample/otherimage:latest"
        );
        assert_eq!(host.spec.image.as_ref().unwrap().signature, None);
    }

    #[test]
    fn test_parse_ostreeremote() {
        const SPEC_FIXTURE: &str = include_str!("fixtures/spec-ostree-remote.yaml");
        let host: Host = serde_yaml::from_str(SPEC_FIXTURE).unwrap();
        assert_eq!(
            host.spec.image.as_ref().unwrap().signature,
            Some(ImageSignature::OstreeRemote("fedora".into()))
        );
    }
}
