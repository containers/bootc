// Copyright 2019 CoreOS, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{bail, ensure, Context, Error, Result};
use camino::Utf8Path;
use fn_error_context::context;
use openssl::hash::{Hasher, MessageDigest};
use openssl::sha;
use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use std::fmt;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::prelude::PermissionsExt;
use std::path::Path;
use std::str::FromStr;

/// The name of the file read by our bootloader config
const FIRSTBOOT: &str = "ignition.firstboot";
/// Kernel argument injected to signal we're on bare metal
pub(crate) const PLATFORM_METAL_KARG: &str = "ignition.platform.id=metal";

/// Ignition-style message digests
#[derive(Debug, Clone, DeserializeFromStr, SerializeDisplay, PartialEq, Eq)]
pub enum IgnitionHash {
    /// SHA-256 digest.
    Sha256(Vec<u8>),
    /// SHA-512 digest.
    Sha512(Vec<u8>),
}

/// Digest implementation.  Helpfully, each digest in openssl::sha has a
/// different type.
enum IgnitionHasher {
    Sha256(sha::Sha256),
    Sha512(sha::Sha512),
}

impl FromStr for IgnitionHash {
    type Err = Error;

    /// Try to parse an hash-digest argument.
    ///
    /// This expects an input value following the `ignition.config.verification.hash`
    /// spec, i.e. `<type>-<value>` format.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let parts: Vec<_> = input.splitn(2, '-').collect();
        if parts.len() != 2 {
            bail!("failed to detect hash-type and digest in '{}'", input);
        }
        let (hash_kind, hex_digest) = (parts[0], parts[1]);

        let hash = match hash_kind {
            "sha256" => {
                let digest = hex::decode(hex_digest).context("decoding hex digest")?;
                ensure!(
                    digest.len().saturating_mul(8) == 256,
                    "wrong digest length ({})",
                    digest.len().saturating_mul(8)
                );
                IgnitionHash::Sha256(digest)
            }
            "sha512" => {
                let digest = hex::decode(hex_digest).context("decoding hex digest")?;
                ensure!(
                    digest.len().saturating_mul(8) == 512,
                    "wrong digest length ({})",
                    digest.len().saturating_mul(8)
                );
                IgnitionHash::Sha512(digest)
            }
            x => bail!("unknown hash type '{}'", x),
        };

        Ok(hash)
    }
}

impl fmt::Display for IgnitionHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, value) = match self {
            Self::Sha256(v) => ("sha256", v),
            Self::Sha512(v) => ("sha512", v),
        };
        write!(f, "{}-{}", kind, hex::encode(value))
    }
}

impl IgnitionHash {
    /// Digest and validate input data.
    pub fn validate(&self, input: &mut impl Read) -> Result<()> {
        let (mut hasher, digest) = match self {
            IgnitionHash::Sha256(val) => (IgnitionHasher::Sha256(sha::Sha256::new()), val),
            IgnitionHash::Sha512(val) => (IgnitionHasher::Sha512(sha::Sha512::new()), val),
        };
        let mut buf = [0u8; 128 * 1024];
        loop {
            match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => match hasher {
                    IgnitionHasher::Sha256(ref mut h) => h.update(&buf[..n]),
                    IgnitionHasher::Sha512(ref mut h) => h.update(&buf[..n]),
                },
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("reading input"),
            };
        }
        let computed = match hasher {
            IgnitionHasher::Sha256(h) => h.finish().to_vec(),
            IgnitionHasher::Sha512(h) => h.finish().to_vec(),
        };

        if &computed != digest {
            bail!(
                "hash mismatch, computed '{}' but expected '{}'",
                hex::encode(computed),
                hex::encode(digest),
            );
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone, Default)]
pub struct Sha256Digest(pub [u8; 32]);

impl TryFrom<Hasher> for Sha256Digest {
    type Error = Error;

    fn try_from(mut hasher: Hasher) -> std::result::Result<Self, Self::Error> {
        let digest = hasher.finish().context("finishing hash")?;
        Ok(Sha256Digest(
            digest.as_ref().try_into().context("converting to SHA256")?,
        ))
    }
}

impl Sha256Digest {
    /// Calculates the SHA256 of a file.
    #[allow(dead_code)]
    pub(crate) fn from_path(path: &Path) -> Result<Self> {
        let mut f = OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("opening {:?}", path))?;

        Self::from_file(&mut f)
    }

    /// Calculates the SHA256 of an opened file. Note that the underlying file descriptor will have
    /// `posix_fadvise` called on it to optimize for sequential reading.
    #[allow(unsafe_code)]
    pub fn from_file(f: &mut std::fs::File) -> Result<Self> {
        // tell kernel to optimize for sequential reading
        if unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_SEQUENTIAL) } < 0 {
            eprintln!(
                "posix_fadvise(SEQUENTIAL) failed (errno {}) -- ignoring...",
                nix::errno::errno()
            );
        }

        Self::from_reader(f)
    }

    /// Calculates the SHA256 of a reader.
    pub fn from_reader(r: &mut impl Read) -> Result<Self> {
        let mut hasher = Hasher::new(MessageDigest::sha256()).context("creating SHA256 hasher")?;
        std::io::copy(r, &mut hasher)?;
        hasher.try_into()
    }

    #[allow(dead_code)]
    pub(crate) fn to_hex_string(&self) -> Result<String> {
        let mut buf: Vec<u8> = Vec::with_capacity(64);
        for i in 0..32 {
            write!(buf, "{:02x}", self.0[i])?;
        }
        Ok(String::from_utf8(buf)?)
    }
}

pub struct WriteHasher<W: Write> {
    writer: W,
    hasher: Hasher,
}

impl<W: Write> WriteHasher<W> {
    #[allow(dead_code)]
    pub fn new(writer: W, hasher: Hasher) -> Self {
        WriteHasher { writer, hasher }
    }

    #[allow(dead_code)]
    pub fn new_sha256(writer: W) -> Result<Self> {
        let hasher = Hasher::new(MessageDigest::sha256()).context("creating SHA256 hasher")?;
        Ok(WriteHasher { writer, hasher })
    }
}

impl<W: Write> Write for WriteHasher<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let n = self.writer.write(buf)?;
        self.hasher.write_all(&buf[..n])?;

        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        self.hasher.flush()?;
        Ok(())
    }
}

impl<W: Write> TryFrom<WriteHasher<W>> for Sha256Digest {
    type Error = Error;

    fn try_from(wrapper: WriteHasher<W>) -> std::result::Result<Self, Self::Error> {
        Sha256Digest::try_from(wrapper.hasher)
    }
}

/// Write the Ignition config.
#[context("Writing ignition")]
pub(crate) fn write_ignition(
    mountpoint: &Utf8Path,
    digest_in: &Option<IgnitionHash>,
    mut config_in: &File,
) -> Result<()> {
    // Verify configuration digest, if any.
    if let Some(digest) = &digest_in {
        digest
            .validate(&mut config_in)
            .context("failed to validate Ignition configuration digest")?;
        config_in
            .seek(io::SeekFrom::Start(0))
            .context("rewinding Ignition configuration file")?;
    };

    // make parent directory
    let mut config_dest = mountpoint.to_path_buf();
    config_dest.push("ignition");
    if !config_dest.is_dir() {
        fs::create_dir_all(&config_dest)
            .with_context(|| format!("creating Ignition config directory {config_dest}"))?;
        // Ignition data may contain secrets; restrict to root
        fs::set_permissions(&config_dest, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("setting file mode for Ignition directory {config_dest}"))?;
    }

    // do the copy
    config_dest.push("config.ign");
    let mut config_out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_dest)
        .with_context(|| format!("opening destination Ignition config {config_dest}"))?;
    // Ignition config may contain secrets; restrict to root
    fs::set_permissions(&config_dest, fs::Permissions::from_mode(0o600)).with_context(|| {
        format!("setting file mode for destination Ignition config {config_dest}")
    })?;
    io::copy(&mut config_in, &mut config_out).context("writing Ignition config")?;

    Ok(())
}

/// Enable Ignition to run on the next boot
#[context("Enabling Ignition firstboot")]
pub(crate) fn enable_firstboot(mountpoint: &Utf8Path) -> Result<()> {
    fs::write(mountpoint.join(FIRSTBOOT), b"").map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ignition_hash_cli_parse() {
        let err_cases = vec!["", "foo-bar", "-bar", "sha512", "sha512-", "sha512-00"];
        for arg in err_cases {
            IgnitionHash::from_str(arg).expect_err(&format!("input: {}", arg));
        }

        let null_digest = "sha512-cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";
        IgnitionHash::from_str(null_digest).unwrap();
    }

    #[test]
    fn test_ignition_hash_validate() {
        let input = vec![b'a', b'b', b'c'];
        let hash_args = [
            (true, "sha256-ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (true, "sha512-ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"),
            (false, "sha256-aa7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (false, "sha512-cdaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f")
        ];
        for (valid, hash_arg) in &hash_args {
            let hasher = IgnitionHash::from_str(&hash_arg).unwrap();
            let mut rd = std::io::Cursor::new(&input);
            assert!(hasher.validate(&mut rd).is_ok() == *valid);
        }
    }
}
