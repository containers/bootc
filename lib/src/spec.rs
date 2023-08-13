//! The definition for host system state.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Representation of a bootc host system
#[derive(
    CustomResource, Serialize, Deserialize, Default, Debug, PartialEq, Eq, Clone, JsonSchema,
)]
#[kube(
    group = "org.containers.bootc",
    version = "v1alpha1",
    kind = "BootcHost",
    struct = "Host",
    namespaced,
    status = "HostStatus",
    derive = "PartialEq",
    derive = "Default"
)]
#[serde(rename_all = "camelCase")]
pub struct HostSpec {
    /// The host image
    pub image: Option<ImageReference>,
    /// Attached configs
    pub configmap_sources: Vec<String>,
}

/// Remote location for a configmap
#[derive(Debug, Clone)]
pub struct ConfigReference {
    /// URL for configmap
    pub url: String,
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
    /// Disable signature verification
    pub signature: ImageSignature,
}

/// The status of the booted image
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageStatus {
    /// The currently booted image
    pub image: ImageReference,
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
    /// Attached configmap objects
    pub configmaps: Vec<String>,
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

    /// Whether or not the current system state is an ostree-based container
    pub is_container: bool,
}
