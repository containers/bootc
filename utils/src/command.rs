//! Helpers intended for [`std::process::Command`] and related structures.

use std::{
    io::{Read, Seek},
    os::unix::process::CommandExt,
    process::Command,
};

use anyhow::{Context, Result};

/// Helpers intended for [`std::process::Command`].
pub trait CommandRunExt {
    /// Log (at debug level) the full child commandline.
    fn log_debug(&mut self) -> &mut Self;

    /// Execute the child process.
    fn run(&mut self) -> Result<()>;

    /// Ensure the child does not outlive the parent.
    fn lifecycle_bind(&mut self) -> &mut Self;

    /// Execute the child process and capture its output. This uses `run` internally
    /// and will return an error if the child process exits abnormally.
    fn run_get_output(&mut self) -> Result<Box<dyn std::io::BufRead>>;

    /// Execute the child process, parsing its stdout as JSON. This uses `run` internally
    /// and will return an error if the child process exits abnormally.
    fn run_and_parse_json<T: serde::de::DeserializeOwned>(&mut self) -> Result<T>;
}

/// Helpers intended for [`std::process::ExitStatus`].
pub trait ExitStatusExt {
    /// If the exit status signals it was not successful, return an error.
    /// Note that we intentionally *don't* include the command string
    /// in the output; we leave it to the caller to add that if they want,
    /// as it may be verbose.
    fn check_status(&mut self, stderr: std::fs::File) -> Result<()>;
}

/// Parse the last chunk (e.g. 1024 bytes) from the provided file,
/// ensure it's UTF-8, and return that value. This function is infallible;
/// if the file cannot be read for some reason, a copy of a static string
/// is returned.
fn last_utf8_content_from_file(mut f: std::fs::File) -> String {
    // u16 since we truncate to just the trailing bytes here
    // to avoid pathological error messages
    const MAX_STDERR_BYTES: u16 = 1024;
    let size = f
        .metadata()
        .map_err(|e| {
            tracing::warn!("failed to fstat: {e}");
        })
        .map(|m| m.len().try_into().unwrap_or(u16::MAX))
        .unwrap_or(0);
    let size = size.min(MAX_STDERR_BYTES);
    let seek_offset = -(size as i32);
    let mut stderr_buf = Vec::with_capacity(size.into());
    // We should never fail to seek()+read() really, but let's be conservative
    let r = match f
        .seek(std::io::SeekFrom::End(seek_offset.into()))
        .and_then(|_| f.read_to_end(&mut stderr_buf))
    {
        Ok(_) => String::from_utf8_lossy(&stderr_buf),
        Err(e) => {
            tracing::warn!("failed seek+read: {e}");
            "<failed to read stderr>".into()
        }
    };
    (&*r).to_owned()
}

impl ExitStatusExt for std::process::ExitStatus {
    fn check_status(&mut self, stderr: std::fs::File) -> Result<()> {
        let stderr_buf = last_utf8_content_from_file(stderr);
        if self.success() {
            return Ok(());
        }
        anyhow::bail!(format!("Subprocess failed: {self:?}\n{stderr_buf}"))
    }
}

impl CommandRunExt for Command {
    /// Synchronously execute the child, and return an error if the child exited unsuccessfully.
    fn run(&mut self) -> Result<()> {
        let stderr = tempfile::tempfile()?;
        self.stderr(stderr.try_clone()?);
        tracing::trace!("exec: {self:?}");
        self.status()?.check_status(stderr)
    }

    #[allow(unsafe_code)]
    fn lifecycle_bind(&mut self) -> &mut Self {
        // SAFETY: This API is safe to call in a forked child.
        unsafe {
            self.pre_exec(|| {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::Term,
                ))
                .map_err(Into::into)
            })
        }
    }

    /// Output a debug-level log message with this command.
    fn log_debug(&mut self) -> &mut Self {
        // We unconditionally log at trace level, so avoid double logging
        if !tracing::enabled!(tracing::Level::TRACE) {
            tracing::debug!("exec: {self:?}");
        }
        self
    }

    fn run_get_output(&mut self) -> Result<Box<dyn std::io::BufRead>> {
        let mut stdout = tempfile::tempfile()?;
        self.stdout(stdout.try_clone()?);
        self.run()?;
        stdout.seek(std::io::SeekFrom::Start(0)).context("seek")?;
        Ok(Box::new(std::io::BufReader::new(stdout)))
    }

    /// Synchronously execute the child, and parse its stdout as JSON.
    fn run_and_parse_json<T: serde::de::DeserializeOwned>(&mut self) -> Result<T> {
        let output = self.run_get_output()?;
        serde_json::from_reader(output).map_err(Into::into)
    }
}

/// Helpers intended for [`tokio::process::Command`].
#[allow(async_fn_in_trait)]
pub trait AsyncCommandRunExt {
    /// Asynchronously execute the child, and return an error if the child exited unsuccessfully.
    async fn run(&mut self) -> Result<()>;
}

impl AsyncCommandRunExt for tokio::process::Command {
    async fn run(&mut self) -> Result<()> {
        let stderr = tempfile::tempfile()?;
        self.stderr(stderr.try_clone()?);
        self.status().await?.check_status(stderr)
    }
}

#[test]
fn command_run_ext() {
    // The basics
    Command::new("true").run().unwrap();
    assert!(Command::new("false").run().is_err());

    // Verify we capture stderr
    let e = Command::new("/bin/sh")
        .args(["-c", "echo expected-this-oops-message 1>&2; exit 1"])
        .run()
        .err()
        .unwrap();
    similar_asserts::assert_eq!(
        e.to_string(),
        "Subprocess failed: ExitStatus(unix_wait_status(256))\nexpected-this-oops-message\n"
    );

    // Ignoring invalid UTF-8
    let e = Command::new("/bin/sh")
        .args([
            "-c",
            r"echo -e 'expected\xf5\x80\x80\x80\x80-foo\xc0bar\xc0\xc0' 1>&2; exit 1",
        ])
        .run()
        .err()
        .unwrap();
    similar_asserts::assert_eq!(
        e.to_string(),
        "Subprocess failed: ExitStatus(unix_wait_status(256))\nexpected�����-foo�bar��\n"
    );
}

#[test]
fn command_run_ext_json() {
    #[derive(serde::Deserialize)]
    struct Foo {
        a: String,
        b: u32,
    }
    let v: Foo = Command::new("echo")
        .arg(r##"{"a": "somevalue", "b": 42}"##)
        .run_and_parse_json()
        .unwrap();
    assert_eq!(v.a, "somevalue");
    assert_eq!(v.b, 42);
}

#[tokio::test]
async fn async_command_run_ext() {
    use tokio::process::Command as AsyncCommand;
    let mut success = AsyncCommand::new("true");
    let mut fail = AsyncCommand::new("false");
    // Run these in parallel just because we can
    let (success, fail) = tokio::join!(success.run(), fail.run(),);
    success.unwrap();
    assert!(fail.is_err());
}
