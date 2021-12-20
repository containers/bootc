//! # APIs bridging OSTree and container images
//!
//! This module contains APIs to bidirectionally map between a single OSTree commit and a container image wrapping it.
//! Because container images are just layers of tarballs, this builds on the [`crate::tar`] module.
//!
//! To emphasize this, the current high level model is that this is a one-to-one mapping - an ostree commit
//! can be exported (wrapped) into a container image, which will have exactly one layer.  Upon import
//! back into an ostree repository, all container metadata except for its digested checksum will be discarded.
//!
//! ## Signatures
//!
//! OSTree supports GPG and ed25519 signatures natively, and it's expected by default that
//! when booting from a fetched container image, one verifies ostree-level signatures.
//! For ostree, a signing configuration is specified via an ostree remote.  In order to
//! pair this configuration together, this library defines a "URL-like" string schema:
//!
//! `ostree-remote-registry:<remotename>:<containerimage>`
//!
//! A concrete instantiation might be e.g.: `ostree-remote-registry:fedora:quay.io/coreos/fedora-coreos:stable`
//!
//! To parse and generate these strings, see [`OstreeImageReference`].
//!
//! ## Layering
//!
//! A key feature of container images is support for layering.  At the moment, support
//! for this is [planned but not implemented](https://github.com/ostreedev/ostree-rs-ext/issues/12).

use anyhow::anyhow;
use std::borrow::Cow;
use std::convert::{TryFrom, TryInto};
use std::ops::Deref;

/// The label injected into a container image that contains the ostree commit SHA-256.
pub const OSTREE_COMMIT_LABEL: &str = "ostree.commit";

/// Our generic catchall fatal error, expected to be converted
/// to a string to output to a terminal or logs.
type Result<T> = anyhow::Result<T>;

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

/// Combination of a signature verification mechanism, and a standard container image reference.
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OstreeImageReference {
    /// The signature verification mechanism.
    pub sigverify: SignatureSource,
    /// The container image reference.
    pub imgref: ImageReference,
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
        let (transport_name, mut name) = value
            .split_once(':')
            .ok_or_else(|| anyhow!("Missing ':' in {}", value))?;
        let transport: Transport = transport_name.try_into()?;
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
        let (first, second) = value
            .split_once(':')
            .ok_or_else(|| anyhow!("Missing ':' in {}", value))?;
        let (sigverify, rest) = match first {
            "ostree-image-signed" => (SignatureSource::ContainerPolicy, Cow::Borrowed(second)),
            "ostree-unverified-image" => (
                SignatureSource::ContainerPolicyAllowInsecure,
                Cow::Borrowed(second),
            ),
            // Shorthand for ostree-unverified-image:registry:
            "ostree-unverified-registry" => (
                SignatureSource::ContainerPolicyAllowInsecure,
                Cow::Owned(format!("registry:{}", second)),
            ),
            // This is a shorthand for ostree-remote-image with registry:
            "ostree-remote-registry" => {
                let (remote, rest) = second
                    .split_once(':')
                    .ok_or_else(|| anyhow!("Missing second ':' in {}", value))?;
                (
                    SignatureSource::OstreeRemote(remote.to_string()),
                    Cow::Owned(format!("registry:{}", rest)),
                )
            }
            "ostree-remote-image" => {
                let (remote, rest) = second
                    .split_once(':')
                    .ok_or_else(|| anyhow!("Missing second ':' in {}", value))?;
                (
                    SignatureSource::OstreeRemote(remote.to_string()),
                    Cow::Borrowed(rest),
                )
            }
            o => {
                return Err(anyhow!("Invalid ostree image reference scheme: {}", o));
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

pub mod deploy;
mod encapsulate;
pub use encapsulate::*;
mod unencapsulate;
pub use unencapsulate::*;
pub(crate) mod ociwriter;
mod skopeo;
pub mod store;

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
        let ir_shorthand =
            OstreeImageReference::try_from("ostree-unverified-registry:quay.io/exampleos/blah")
                .unwrap();
        assert_eq!(&ir_shorthand, &ir);
    }
}
