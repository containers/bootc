//! # APIs bridging OSTree and container images
//!
//! This crate contains APIs to bidirectionally map
//! between OSTree repositories and container images.

//#![deny(missing_docs)]
// Good defaults
#![forbid(unused_must_use)]
#![deny(unsafe_code)]

use anyhow::anyhow;
use std::borrow::Cow;
use std::convert::{TryFrom, TryInto};
use std::ops::Deref;

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
    /// Local container storage (`containers-storage:`)
    ContainerStorage,
}

/// Combination of a remote image reference and transport.
///
/// For example,
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReference {
    /// The storage and transport for the image
    pub transport: Transport,
    /// The image name (e.g. `quay.io/somerepo/someimage:latest`)
    pub name: String,
}

/// Policy for signature verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureSource {
    /// Fetches will use the named ostree remote for signature verification of the ostree commit.
    OstreeRemote(String),
    /// Fetches will defer to the `containers-policy.json`, but we make a best effort to reject `default: insecureAcceptAnything` policy.
    ContainerPolicy,
    /// NOT RECOMMENDED.  Fetches will defer to the `containers-policy.json` default which is usually `insecureAcceptAnything`.
    ContainerPolicyAllowInsecure,
}

/// Combination of an ostree remote (for signature verification) and an image reference.
///
/// For example, myremote:docker://quay.io/somerepo/someimage.latest
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OstreeImageReference {
    /// The ostree remote name.
    /// This will be used for signature verification.
    pub sigverify: SignatureSource,
    /// The container image reference.
    pub imgref: ImageReference,
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

impl OstreeImageReference {
    /// Create a new `OstreeImageReference` that refers to a specific digest.
    ///
    /// ```rust
    /// use std::convert::TryInto;
    /// let r: ostree_ext::container::OstreeImageReference = "ostree-remote-image:myremote:docker://quay.io/exampleos/exampleos:latest".try_into().unwrap();
    /// let n = r.with_digest("sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
    /// assert_eq!(n.imgref.name, "quay.io/exampleos/exampleos@sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
    /// ```
    pub fn with_digest(&self, digest: &str) -> Self {
        Self {
            sigverify: self.sigverify.clone(),
            imgref: self.imgref.with_digest(digest),
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
            "containers-storage" => Self::ContainerStorage,
            o => return Err(anyhow!("Unknown transport '{}'", o)),
        })
    }
}

impl TryFrom<&str> for ImageReference {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        let mut parts = value.splitn(2, ':');
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

impl TryFrom<&str> for SignatureSource {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "ostree-image-signed" => Ok(Self::ContainerPolicy),
            "ostree-unverified-image" => Ok(Self::ContainerPolicyAllowInsecure),
            o => match o.strip_prefix("ostree-remote-image:") {
                Some(rest) => Ok(Self::OstreeRemote(rest.to_string())),
                _ => Err(anyhow!("Invalid signature source: {}", o)),
            },
        }
    }
}

impl TryFrom<&str> for OstreeImageReference {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        let mut parts = value.splitn(2, ':');
        // Safety: Split always returns at least one value.
        let first = parts.next().unwrap();
        let second = parts
            .next()
            .ok_or_else(|| anyhow!("Missing ':' in {}", value))?;
        let (sigverify, rest) = match first {
            "ostree-image-signed" => (SignatureSource::ContainerPolicy, Cow::Borrowed(second)),
            "ostree-unverified-image" => (
                SignatureSource::ContainerPolicyAllowInsecure,
                Cow::Borrowed(second),
            ),
            // This is a shorthand for ostree-remote-image with registry:
            "ostree-remote-registry" => {
                let mut subparts = second.splitn(2, ':');
                // Safety: Split always returns at least one value.
                let remote = subparts.next().unwrap();
                let rest = subparts
                    .next()
                    .ok_or_else(|| anyhow!("Missing second ':' in {}", value))?;
                (
                    SignatureSource::OstreeRemote(remote.to_string()),
                    Cow::Owned(format!("registry:{}", rest)),
                )
            }
            "ostree-remote-image" => {
                let mut subparts = second.splitn(2, ':');
                // Safety: Split always returns at least one value.
                let remote = subparts.next().unwrap();
                let second = Cow::Borrowed(
                    subparts
                        .next()
                        .ok_or_else(|| anyhow!("Missing second ':' in {}", value))?,
                );
                (SignatureSource::OstreeRemote(remote.to_string()), second)
            }
            o => {
                return Err(anyhow!("Invalid signature source: {}", o));
            }
        };
        let imgref = rest.deref().try_into()?;
        Ok(Self { sigverify, imgref })
    }
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            // TODO once skopeo supports this, canonicalize as registry:
            Self::Registry => "docker://",
            Self::OciArchive => "oci-archive:",
            Self::OciDir => "oci:",
            Self::ContainerStorage => "containers-storage:",
        };
        f.write_str(s)
    }
}

impl std::fmt::Display for ImageReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.transport, self.name)
    }
}

impl std::fmt::Display for OstreeImageReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.sigverify {
            SignatureSource::OstreeRemote(r) => {
                write!(f, "ostree-remote-image:{}:{}", r, self.imgref)
            }
            SignatureSource::ContainerPolicy => write!(f, "ostree-image-signed:{}", self.imgref),
            SignatureSource::ContainerPolicyAllowInsecure => {
                write!(f, "ostree-unverified-image:{}", self.imgref)
            }
        }
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
    const VALID_IRS: &[&str] = &[
        "containers-storage:localhost/someimage",
        "docker://quay.io/exampleos/blah:sometag",
    ];

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

        for &v in VALID_IRS {
            ImageReference::try_from(v).unwrap();
        }

        for &v in INVALID_IRS {
            if ImageReference::try_from(v).is_ok() {
                panic!("Should fail to parse: {}", v)
            }
        }
        let ir: ImageReference = "oci:somedir".try_into().unwrap();
        assert_eq!(ir.transport, Transport::OciDir);
        assert_eq!(ir.name, "somedir");
    }

    #[test]
    fn test_ostreeimagereference() {
        // Test both long form `ostree-remote-image:$myremote:registry` and the
        // shorthand `ostree-remote-registry:$myremote`.
        let ir_s = "ostree-remote-image:myremote:registry:quay.io/exampleos/blah";
        let ir_registry = "ostree-remote-registry:myremote:quay.io/exampleos/blah";
        for &ir_s in &[ir_s, ir_registry] {
            let ir: OstreeImageReference = ir_s.try_into().unwrap();
            assert_eq!(
                ir.sigverify,
                SignatureSource::OstreeRemote("myremote".to_string())
            );
            assert_eq!(ir.imgref.transport, Transport::Registry);
            assert_eq!(ir.imgref.name, "quay.io/exampleos/blah");
            assert_eq!(
                ir.to_string(),
                "ostree-remote-image:myremote:docker://quay.io/exampleos/blah"
            );
        }

        let ir: OstreeImageReference = ir_s.try_into().unwrap();
        // test our Eq implementation
        assert_eq!(&ir, &OstreeImageReference::try_from(ir_registry).unwrap());

        let digested = ir
            .with_digest("sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
        assert_eq!(digested.imgref.name, "quay.io/exampleos/blah@sha256:41af286dc0b172ed2f1ca934fd2278de4a1192302ffa07087cea2682e7d372e3");
        assert_eq!(digested.with_digest("sha256:52f562806109f5746be31ccf21f5569fd2ce8c32deb0d14987b440ed39e34e20").imgref.name, "quay.io/exampleos/blah@sha256:52f562806109f5746be31ccf21f5569fd2ce8c32deb0d14987b440ed39e34e20");

        let ir_s = "ostree-image-signed:docker://quay.io/exampleos/blah";
        let ir: OstreeImageReference = ir_s.try_into().unwrap();
        assert_eq!(ir.sigverify, SignatureSource::ContainerPolicy);
        assert_eq!(ir.imgref.transport, Transport::Registry);
        assert_eq!(ir.imgref.name, "quay.io/exampleos/blah");
        assert_eq!(
            ir.to_string(),
            "ostree-image-signed:docker://quay.io/exampleos/blah"
        );

        let ir_s = "ostree-unverified-image:docker://quay.io/exampleos/blah";
        let ir: OstreeImageReference = ir_s.try_into().unwrap();
        assert_eq!(ir.sigverify, SignatureSource::ContainerPolicyAllowInsecure);
        assert_eq!(ir.imgref.transport, Transport::Registry);
        assert_eq!(ir.imgref.name, "quay.io/exampleos/blah");
        assert_eq!(
            ir.to_string(),
            "ostree-unverified-image:docker://quay.io/exampleos/blah"
        );
    }
}
