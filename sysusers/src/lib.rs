//! Parse and generate systemd sysusers.d entries.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;

use thiserror::Error;

/// An error when translating tmpfiles.d.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("I/O error on {path}: {err}")]
    PathIo { path: PathBuf, err: std::io::Error },
}

/// The type of Result.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {}
