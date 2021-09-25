//! APIs to write a tarball stream into an OSTree commit.
//!
//! This functionality already exists in libostree mostly,
//! this API adds a higher level, more ergonomic Rust frontend
//! to it.
//!
//! In the future, this may also evolve into parsing the tar
//! stream in Rust, not in C.

use crate::cmdext::CommandRedirectionExt;
use crate::Result;
use anyhow::anyhow;
use std::os::unix::prelude::AsRawFd;
use tokio::io::AsyncReadExt;
use tracing::instrument;

/// Configuration for tar layer commits.
#[derive(Debug, Default)]
pub struct WriteTarOptions<'a> {
    /// Base ostree commit hash
    pub base: Option<&'a str>,
    /// Enable SELinux labeling from the base commit
    /// Requires the `base` option.
    pub selinux: bool,
}

/// Write the contents of a tarball as an ostree commit.
#[allow(unsafe_code)] // For raw fd bits
#[instrument(skip(repo, src))]
pub async fn write_tar(
    repo: &ostree::Repo,
    mut src: impl tokio::io::AsyncRead + Send + Unpin + 'static,
    refname: &str,
    options: Option<WriteTarOptions<'_>>,
) -> Result<String> {
    use std::process::Stdio;
    let options = options.unwrap_or_default();
    let mut c = std::process::Command::new("ostree");
    let repofd = repo.dfd_as_file()?;
    {
        let c = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(&["commit"]);
        c.take_fd_n(repofd.as_raw_fd(), 3);
        c.arg("--repo=/proc/self/fd/3");
        if let Some(base) = options.base {
            if options.selinux {
                c.arg("--selinux-policy-from-base");
            }
            c.arg(&format!("--tree=ref={}", base));
        }
        c.args(&[
            "--no-bindings",
            "--tar-autocreate-parents",
            r#"--tar-pathname-filter=^etc(.*),usr/etc\1"#,
            "--tree=tar=/proc/self/fd/0",
            "--branch",
            refname,
        ]);
    }
    let mut c = tokio::process::Command::from(c);
    c.kill_on_drop(true);
    let mut r = c.spawn()?;
    // Safety: We passed piped() for all of these
    let mut child_stdin = r.stdin.take().unwrap();
    let mut child_stdout = r.stdout.take().unwrap();
    let mut child_stderr = r.stderr.take().unwrap();
    // Copy our input to child stdout
    let input_copier = async move {
        let _n = tokio::io::copy(&mut src, &mut child_stdin).await?;
        drop(child_stdin);
        Ok::<_, anyhow::Error>(())
    };
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

    let (_, (child_stdout, child_stderr)) = tokio::try_join!(input_copier, output_copier)?;
    let status = r.wait().await?;
    if !status.success() {
        return Err(anyhow!(
            "Failed to commit tar: {:?}: {}",
            status,
            child_stderr
        ));
    }
    // TODO: trim string in place
    let s = child_stdout.trim();
    Ok(s.to_string())
}
