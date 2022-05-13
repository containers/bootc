//! APIs to write a tarball stream into an OSTree commit.
//!
//! This functionality already exists in libostree mostly,
//! this API adds a higher level, more ergonomic Rust frontend
//! to it.
//!
//! In the future, this may also evolve into parsing the tar
//! stream in Rust, not in C.

use crate::Result;
use anyhow::{anyhow, Context};
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use cap_std_ext::cmdext::CapStdExtCommandExt;
use cap_std_ext::rustix;
use ostree::gio;
use ostree::prelude::FileExt;
use rustix::fd::FromFd;
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tracing::instrument;

/// Copy a tar entry to a new tar archive, optionally using a different filesystem path.
pub(crate) fn copy_entry(
    entry: tar::Entry<impl std::io::Read>,
    dest: &mut tar::Builder<impl std::io::Write>,
    path: Option<&Path>,
) -> Result<()> {
    // Make copies of both the header and path, since that's required for the append APIs
    let path = if let Some(path) = path {
        path.to_owned()
    } else {
        (&*entry.path()?).to_owned()
    };
    let mut header = entry.header().clone();

    // Need to use the entry.link_name() not the header.link_name()
    // api as the header api does not handle long paths:
    // https://github.com/alexcrichton/tar-rs/issues/192
    match entry.header().entry_type() {
        tar::EntryType::Link | tar::EntryType::Symlink => {
            let target = entry.link_name()?.ok_or_else(|| anyhow!("Invalid link"))?;
            dest.append_link(&mut header, path, target)
        }
        _ => dest.append_data(&mut header, path, entry),
    }
    .map_err(Into::into)
}

/// Configuration for tar layer commits.
#[derive(Debug, Default)]
pub struct WriteTarOptions {
    /// Base ostree commit hash
    pub base: Option<String>,
    /// Enable SELinux labeling from the base commit
    /// Requires the `base` option.
    pub selinux: bool,
}

/// The result of writing a tar stream.
///
/// This includes some basic data on the number of files that were filtered
/// out because they were not in `/usr`.
#[derive(Debug, Default)]
pub struct WriteTarResult {
    /// The resulting OSTree commit SHA-256.
    pub commit: String,
    /// Number of paths in a prefix (e.g. `/var` or `/boot`) which were discarded.
    pub filtered: BTreeMap<String, u32>,
}

// Copy of logic from https://github.com/ostreedev/ostree/pull/2447
// to avoid waiting for backport + releases
fn sepolicy_from_base(repo: &ostree::Repo, base: &str) -> Result<tempfile::TempDir> {
    let cancellable = gio::NONE_CANCELLABLE;
    let policypath = "usr/etc/selinux";
    let tempdir = tempfile::tempdir()?;
    let (root, _) = repo.read_commit(base, cancellable)?;
    let policyroot = root.resolve_relative_path(policypath);
    if policyroot.query_exists(cancellable) {
        let policydest = tempdir.path().join(policypath);
        std::fs::create_dir_all(policydest.parent().unwrap())?;
        let opts = ostree::RepoCheckoutAtOptions {
            mode: ostree::RepoCheckoutMode::User,
            subpath: Some(Path::new(policypath).to_owned()),
            ..Default::default()
        };
        repo.checkout_at(Some(&opts), ostree::AT_FDCWD, policydest, base, cancellable)?;
    }
    Ok(tempdir)
}

#[derive(Debug)]
enum NormalizedPathResult<'a> {
    Filtered(&'a str),
    Normal(Utf8PathBuf),
}

fn normalize_validate_path(path: &Utf8Path) -> Result<NormalizedPathResult<'_>> {
    // This converts e.g. `foo//bar/./baz` into `foo/bar/baz`.
    let mut components = path
        .components()
        .map(|part| {
            match part {
                // Convert absolute paths to relative
                camino::Utf8Component::RootDir => Ok(camino::Utf8Component::CurDir),
                // Allow ./ and regular parts
                camino::Utf8Component::Normal(_) | camino::Utf8Component::CurDir => Ok(part),
                // Barf on Windows paths as well as Unix path uplinks `..`
                _ => Err(anyhow!("Invalid path: {}", path)),
            }
        })
        .peekable();
    let mut ret = Utf8PathBuf::new();
    // Insert a leading `./` if not present
    if let Some(Ok(camino::Utf8Component::Normal(_))) = components.peek() {
        ret.push(camino::Utf8Component::CurDir);
    }
    let mut found_first = false;
    for part in components {
        let part = part?;
        if !found_first {
            if let Utf8Component::Normal(part) = part {
                found_first = true;
                // Now, rewrite /etc -> /usr/etc, and discard everything not in /usr.
                match part {
                    "usr" => ret.push(part),
                    "etc" => {
                        ret.push("usr/etc");
                    }
                    o => return Ok(NormalizedPathResult::Filtered(o)),
                }
            } else {
                ret.push(part);
            }
        } else {
            ret.push(part);
        }
    }

    Ok(NormalizedPathResult::Normal(ret))
}

/// Perform various filtering on imported tar archives.
///  - Move /etc to /usr/etc
///  - Entirely drop files not in /usr
///
/// This also acts as a Rust "pre-parser" of the tar archive, hopefully
/// catching anything corrupt that might be exploitable from the C libarchive side.
/// Remember that we're parsing this while we're downloading it, and in order
/// to verify integrity we rely on the total sha256 of the blob, so all content
/// written before then must be considered untrusted.
pub(crate) fn filter_tar(
    src: impl std::io::Read,
    dest: impl std::io::Write,
) -> Result<BTreeMap<String, u32>> {
    let src = std::io::BufReader::new(src);
    let mut src = tar::Archive::new(src);
    let dest = BufWriter::new(dest);
    let mut dest = tar::Builder::new(dest);
    let mut filtered = BTreeMap::new();

    let ents = src.entries()?;
    for entry in ents {
        let entry = entry?;
        let path = entry.path()?;
        let path: &Utf8Path = (&*path).try_into()?;

        let normalized = match normalize_validate_path(path)? {
            NormalizedPathResult::Filtered(path) => {
                if let Some(v) = filtered.get_mut(path) {
                    *v += 1;
                } else {
                    filtered.insert(path.to_string(), 1);
                }
                continue;
            }
            NormalizedPathResult::Normal(path) => path,
        };

        copy_entry(entry, &mut dest, Some(normalized.as_std_path()))?;
    }
    dest.into_inner()?.flush()?;
    Ok(filtered)
}

/// Asynchronous wrapper for filter_tar()
async fn filter_tar_async(
    src: impl AsyncRead + Send + 'static,
    mut dest: impl AsyncWrite + Send + Unpin,
) -> Result<BTreeMap<String, u32>> {
    let (tx_buf, mut rx_buf) = tokio::io::duplex(8192);
    let src = Box::pin(src);
    let tar_transformer = tokio::task::spawn_blocking(move || -> Result<_> {
        let src = tokio_util::io::SyncIoBridge::new(src);
        let dest = tokio_util::io::SyncIoBridge::new(tx_buf);
        filter_tar(src, dest)
    });
    let copier = tokio::io::copy(&mut rx_buf, &mut dest);
    let (r, v) = tokio::join!(tar_transformer, copier);
    let _v: u64 = v?;
    r?
}

/// Write the contents of a tarball as an ostree commit.
#[allow(unsafe_code)] // For raw fd bits
#[instrument(skip(repo, src))]
pub async fn write_tar(
    repo: &ostree::Repo,
    src: impl tokio::io::AsyncRead + Send + Unpin + 'static,
    refname: &str,
    options: Option<WriteTarOptions>,
) -> Result<WriteTarResult> {
    let repo = repo.clone();
    let options = options.unwrap_or_default();
    let sepolicy = if options.selinux {
        if let Some(base) = options.base {
            Some(sepolicy_from_base(&repo, &base).context("tar: Preparing sepolicy")?)
        } else {
            None
        }
    } else {
        None
    };
    let mut c = std::process::Command::new("ostree");
    let repofd = repo.dfd_as_file()?;
    let repofd = Arc::new(rustix::io::OwnedFd::from_into_fd(repofd));
    {
        let c = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(&["commit"]);
        c.take_fd_n(repofd.clone(), 3);
        c.arg("--repo=/proc/self/fd/3");
        if let Some(sepolicy) = sepolicy.as_ref() {
            c.arg("--selinux-policy");
            c.arg(sepolicy.path());
        }
        c.arg(&format!(
            "--add-metadata-string=ostree.importer.version={}",
            env!("CARGO_PKG_VERSION")
        ));
        c.args(&[
            "--no-bindings",
            "--tar-autocreate-parents",
            "--tree=tar=/proc/self/fd/0",
            "--branch",
            refname,
        ]);
    }
    let mut c = tokio::process::Command::from(c);
    c.kill_on_drop(true);
    let mut r = c.spawn()?;
    // Safety: We passed piped() for all of these
    let child_stdin = r.stdin.take().unwrap();
    let mut child_stdout = r.stdout.take().unwrap();
    let mut child_stderr = r.stderr.take().unwrap();
    // Copy the filtered tar stream to child stdin
    let filtered_result = filter_tar_async(src, child_stdin);
    // Gather stdout/stderr to buffers
    let output_copier = async move {
        let mut child_stdout_buf = String::new();
        let mut child_stderr_buf = String::new();
        let (_a, _b) = tokio::try_join!(
            child_stdout.read_to_string(&mut child_stdout_buf),
            child_stderr.read_to_string(&mut child_stderr_buf)
        )?;
        Ok::<_, anyhow::Error>((child_stdout_buf, child_stderr_buf))
    };

    let (filtered_result, (child_stdout, child_stderr)) =
        tokio::try_join!(filtered_result, output_copier)?;
    let status = r.wait().await?;
    // Ensure this lasted until the process exited
    drop(sepolicy);
    if !status.success() {
        return Err(anyhow!(
            "Failed to commit tar: {:?}: {}",
            status,
            child_stderr
        ));
    }
    // TODO: trim string in place
    let s = child_stdout.trim();
    Ok(WriteTarResult {
        commit: s.to_string(),
        filtered: filtered_result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_normalize_path() {
        let valid = &[
            ("/usr/bin/blah", "./usr/bin/blah"),
            ("usr/bin/blah", "./usr/bin/blah"),
            ("usr///share/.//blah", "./usr/share/blah"),
            ("./", "."),
        ];
        for &(k, v) in valid {
            let r = normalize_validate_path(k.into()).unwrap();
            match r {
                NormalizedPathResult::Filtered(o) => {
                    panic!("Case {} should not be filtered as {}", k, o)
                }
                NormalizedPathResult::Normal(p) => {
                    assert_eq!(v, p.as_str());
                }
            }
        }
        let filtered = &[
            ("/boot/vmlinuz", "boot"),
            ("var/lib/blah", "var"),
            ("./var/lib/blah", "var"),
        ];
        for &(k, v) in filtered {
            match normalize_validate_path(k.into()).unwrap() {
                NormalizedPathResult::Filtered(f) => {
                    assert_eq!(v, f);
                }
                NormalizedPathResult::Normal(_) => {
                    panic!("{} should be filtered", k)
                }
            }
        }
        let errs = &["usr/foo/../../bar"];
        for &k in errs {
            assert!(normalize_validate_path(k.into()).is_err());
        }
    }

    #[tokio::test]
    async fn tar_filter() -> Result<()> {
        let tempd = tempfile::tempdir()?;
        let rootfs = &tempd.path().join("rootfs");
        std::fs::create_dir_all(rootfs.join("etc/systemd/system"))?;
        std::fs::write(rootfs.join("etc/systemd/system/foo.service"), "fooservice")?;
        std::fs::write(rootfs.join("blah"), "blah")?;
        let rootfs_tar_path = &tempd.path().join("rootfs.tar");
        let rootfs_tar = std::fs::File::create(rootfs_tar_path)?;
        let mut rootfs_tar = tar::Builder::new(rootfs_tar);
        rootfs_tar.append_dir_all(".", rootfs)?;
        let _ = rootfs_tar.into_inner()?;
        let mut dest = Vec::new();
        let src = tokio::io::BufReader::new(tokio::fs::File::open(rootfs_tar_path).await?);
        filter_tar_async(src, &mut dest).await?;
        let dest = dest.as_slice();
        let mut final_tar = tar::Archive::new(Cursor::new(dest));
        let destdir = &tempd.path().join("destdir");
        final_tar.unpack(destdir)?;
        assert!(destdir.join("usr/etc/systemd/system/foo.service").exists());
        assert!(!destdir.join("blah").exists());
        Ok(())
    }
}
