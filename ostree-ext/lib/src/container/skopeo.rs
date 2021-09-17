//! Fork skopeo as a subprocess

use super::Result;
use anyhow::Context;
use serde::Deserialize;
use std::process::Stdio;
use tokio::process::Command;

// See `man containers-policy.json` and
// https://github.com/containers/image/blob/main/signature/policy_types.go
// Ideally we add something like `skopeo pull --disallow-insecure-accept-anything`
// but for now we parse the policy.
const POLICY_PATH: &str = "/etc/containers/policy.json";
const INSECURE_ACCEPT_ANYTHING: &str = "insecureAcceptAnything";

bitflags::bitflags! {
    pub(crate) struct SkopeoFeatures: u32 {
        const COPY_DIGESTFILE = 0b00000001;
    }
}

lazy_static::lazy_static! {
    static ref SKOPEO_FEATURES: Result<SkopeoFeatures> = {
        let mut features = SkopeoFeatures::empty();
        let c = std::process::Command::new("skopeo")
            .args(&["copy", "--help"])
            .stderr(std::process::Stdio::piped())
            .output()?;
        let stdout = String::from_utf8_lossy(&c.stderr);
        if stdout.contains("--digestfile") {
            features.insert(SkopeoFeatures::COPY_DIGESTFILE);
        }
        Ok(features)
    };
}

pub(crate) fn skopeo_has_features(wanted: SkopeoFeatures) -> Result<bool> {
    match &*SKOPEO_FEATURES {
        Ok(found) => Ok(found.intersects(wanted)),
        Err(e) => Err(anyhow::Error::msg(e)),
    }
}

#[derive(Deserialize)]
struct PolicyEntry {
    #[serde(rename = "type")]
    ty: String,
}
#[derive(Deserialize)]
struct ContainerPolicy {
    default: Option<Vec<PolicyEntry>>,
}

impl ContainerPolicy {
    fn is_default_insecure(&self) -> bool {
        if let Some(default) = self.default.as_deref() {
            match default.split_first() {
                Some((v, &[])) => v.ty == INSECURE_ACCEPT_ANYTHING,
                _ => false,
            }
        } else {
            false
        }
    }
}

pub(crate) fn container_policy_is_default_insecure() -> Result<bool> {
    let r = std::io::BufReader::new(std::fs::File::open(POLICY_PATH)?);
    let policy: ContainerPolicy = serde_json::from_reader(r)?;
    Ok(policy.is_default_insecure())
}

/// Create a Command builder for skopeo.
pub(crate) fn new_cmd() -> tokio::process::Command {
    let mut cmd = Command::new("skopeo");
    cmd.stdin(Stdio::null());
    cmd.kill_on_drop(true);
    cmd
}

/// Spawn the child process
pub(crate) fn spawn(mut cmd: Command) -> Result<tokio::process::Child> {
    let cmd = cmd.stdin(Stdio::null()).stderr(Stdio::piped());
    cmd.spawn().context("Failed to exec skopeo")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Default value as of the Fedora 34 containers-common-1-21.fc34.noarch package.
    const DEFAULT_POLICY: &str = indoc::indoc! {r#"
    {
        "default": [
            {
                "type": "insecureAcceptAnything"
            }
        ],
        "transports":
            {
                "docker-daemon":
                    {
                        "": [{"type":"insecureAcceptAnything"}]
                    }
            }
    }
    "#};

    // Stripped down copy from the manual.
    const REASONABLY_LOCKED_DOWN: &str = indoc::indoc! { r#"
    {
        "default": [{"type": "reject"}],
        "transports": {
            "dir": {
                "": [{"type": "insecureAcceptAnything"}]
            },
            "atomic": {
                "hostname:5000/myns/official": [
                    {
                        "type": "signedBy",
                        "keyType": "GPGKeys",
                        "keyPath": "/path/to/official-pubkey.gpg"
                    }
                ]
            }
        }
    }
    "#};

    #[test]
    fn policy_is_insecure() {
        let p: ContainerPolicy = serde_json::from_str(DEFAULT_POLICY).unwrap();
        assert!(p.is_default_insecure());
        for &v in &["{}", REASONABLY_LOCKED_DOWN] {
            let p: ContainerPolicy = serde_json::from_str(v).unwrap();
            assert!(!p.is_default_insecure());
        }
    }
}
