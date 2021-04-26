//! # APIs bridging OSTree and container images
//!
//! This crate contains APIs to bidirectionally map
//! between OSTree repositories and container images.

//#![deny(missing_docs)]
// Good defaults
#![forbid(unused_must_use)]
#![deny(unsafe_code)]

use anyhow::anyhow;
use std::convert::{TryFrom, TryInto};

/// The label injected into a container image that contains the ostree commit SHA-256.
pub const OSTREE_COMMIT_LABEL: &str = "ostree.commit";

/// Our generic catchall fatal error, expected to be converted
/// to a string to output to a terminal or logs.
type Result<T> = anyhow::Result<T>;

/// Information about the image manifest.
pub struct OstreeContainerManifestInfo {
    /// The manifest digest (`sha256:<value>`)
    pub manifest_digest: String,
}

/// A backend/transport for OCI/Docker images.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// A remote Docker/OCI registry (`registry:` or `docker://`)
    Registry,
    /// A local OCI directory (`oci:`)
    OciDir,
    /// A local OCI archive tarball (`oci-archive:`)
    OciArchive,
}

/// Combination of a remote image reference and transport.
///
/// For example,
#[derive(Debug)]
pub struct ImageReference {
    /// The storage and transport for the image
    pub transport: Transport,
    /// The image name (e.g. `quay.io/somerepo/someimage:latest`)
    pub name: String,
}

impl ImageReference {
    /// Create a new `ImageReference` that refers to a specific digest.
    ///
    /// ```rust
    /// use std::convert::TryInto;
    /// let r: ostree_ext::container::ImageReference = "docker://quay.io/exampleos/exampleos:latest".try_into().unwrap();
    /// let n = r.with_digest("sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
    /// assert_eq!(n.name, "quay.io/exampleos/exampleos@sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
    /// ```
    pub fn with_digest(&self, digest: &str) -> Self {
        let name = self.name.as_str();
        let name = if let Some(idx) = name.rfind('@') {
            name.split_at(idx).0
        } else if let Some(idx) = name.rfind(':') {
            name.split_at(idx).0
        } else {
            name
        };
        Self {
            transport: self.transport,
            name: format!("{}@{}", name, digest),
        }
    }
}

impl TryFrom<&str> for Transport {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        Ok(match value {
            "registry" | "docker" => Self::Registry,
            "oci" => Self::OciDir,
            "oci-archive" => Self::OciArchive,
            o => return Err(anyhow!("Unknown transport '{}'", o)),
        })
    }
}

impl TryFrom<&str> for ImageReference {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        let mut parts = value.splitn(2, ":");
        let transport_name = parts.next().unwrap();
        let transport: Transport = transport_name.try_into()?;
        let mut name = parts
            .next()
            .ok_or_else(|| anyhow!("Missing ':' in {}", value))?;
        if name.is_empty() {
            return Err(anyhow!("Invalid empty name in {}", value));
        }
        if transport_name == "docker" {
            name = name
                .strip_prefix("//")
                .ok_or_else(|| anyhow!("Missing // in docker:// in {}", value))?;
        }
        Ok(Self {
            transport,
            name: name.to_string(),
        })
    }
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            // TODO once skopeo supports this, canonicalize as registry:
            Self::Registry => "docker://",
            Self::OciArchive => "oci-archive:",
            Self::OciDir => "oci:",
        };
        f.write_str(s)
    }
}

impl std::fmt::Display for ImageReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.transport, self.name)
    }
}

mod export;
pub use export::*;
mod import;
pub use import::*;
mod oci;
mod skopeo;

#[cfg(test)]
mod tests {
    use super::*;

    const INVALID_IRS: &[&str] = &["", "foo://", "docker:blah", "registry:", "foo:bar"];

    #[test]
    fn test_imagereference() {
        let ir: ImageReference = "registry:quay.io/exampleos/blah".try_into().unwrap();
        assert_eq!(ir.transport, Transport::Registry);
        assert_eq!(ir.name, "quay.io/exampleos/blah");
        assert_eq!(ir.to_string(), "docker://quay.io/exampleos/blah");

        let digested = ir
            .with_digest("sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
        assert_eq!(digested.name, "quay.io/exampleos/blah@sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
        assert_eq!(digested.with_digest("sha256:52f562806109f5746be31ccf21f5569fd2ce8c32deb0d14987b440ed39e34e20").name, "quay.io/exampleos/blah@sha256:52f562806109f5746be31ccf21f5569fd2ce8c32deb0d14987b440ed39e34e20");

        let with_tag: ImageReference = "registry:quay.io/exampleos/blah:sometag"
            .try_into()
            .unwrap();
        let digested = with_tag
            .with_digest("sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
        assert_eq!(digested.name, "quay.io/exampleos/blah@sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");

        for &v in INVALID_IRS {
            match ImageReference::try_from(v) {
                Ok(_) => panic!("Should fail to parse: {}", v),
                Err(_) => {}
            }
        }
        let ir: ImageReference = "oci:somedir".try_into().unwrap();
        assert_eq!(ir.transport, Transport::OciDir);
        assert_eq!(ir.name, "somedir");
    }
}
