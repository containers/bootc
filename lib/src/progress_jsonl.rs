//! Output progress data using the json-lines format. For more information
//! see <https://jsonlines.org/>.

use anyhow::Result;
use fn_error_context::context;
use serde::Serialize;
use std::borrow::Cow;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::net::unix::pipe::Sender;
use tokio::sync::Mutex;

// Maximum number of times per second that an event will be written.
const REFRESH_HZ: u16 = 5;

pub const API_VERSION: &str = "org.containers.bootc.progress/v1";

/// An incremental update to e.g. a container image layer download.
/// The first time a given "subtask" name is seen, a new progress bar should be created.
/// If bytes == bytes_total, then the subtask is considered complete.
#[derive(Debug, serde::Serialize, serde::Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubTaskBytes<'t> {
    /// A machine readable type for the task (used for i18n).
    /// (e.g., "ostree_chunk", "ostree_derived")
    #[serde(borrow)]
    pub subtask: Cow<'t, str>,
    /// A human readable description of the task if i18n is not available.
    /// (e.g., "OSTree Chunk:", "Derived Layer:")
    #[serde(borrow)]
    pub description: Cow<'t, str>,
    /// A human and machine readable identifier for the task
    /// (e.g., ostree chunk/layer hash).
    #[serde(borrow)]
    pub id: Cow<'t, str>,
    /// The number of bytes fetched by a previous run (e.g., zstd_chunked).
    pub bytes_cached: u64,
    /// Updated byte level progress
    pub bytes: u64,
    /// Total number of bytes
    pub bytes_total: u64,
}

/// Marks the beginning and end of a dictrete step
#[derive(Debug, serde::Serialize, serde::Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubTaskStep<'t> {
    /// A machine readable type for the task (used for i18n).
    /// (e.g., "ostree_chunk", "ostree_derived")
    #[serde(borrow)]
    pub subtask: Cow<'t, str>,
    /// A human readable description of the task if i18n is not available.
    /// (e.g., "OSTree Chunk:", "Derived Layer:")
    #[serde(borrow)]
    pub description: Cow<'t, str>,
    /// A human and machine readable identifier for the task
    /// (e.g., ostree chunk/layer hash).
    #[serde(borrow)]
    pub id: Cow<'t, str>,
    /// Starts as false when beginning to execute and turns true when completed.
    pub completed: bool,
}

/// An event emitted as JSON.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "type",
    rename_all = "PascalCase",
    rename_all_fields = "camelCase"
)]
pub enum Event<'t> {
    /// An incremental update to a container image layer download
    ProgressBytes {
        /// The version of the progress event format.
        #[serde(borrow)]
        api_version: Cow<'t, str>,
        /// A machine readable type (e.g., pulling) for the task (used for i18n
        /// and UI customization).
        #[serde(borrow)]
        task: Cow<'t, str>,
        /// A human readable description of the task if i18n is not available.
        #[serde(borrow)]
        description: Cow<'t, str>,
        /// A human and machine readable unique identifier for the task
        /// (e.g., the image name). For tasks that only happen once,
        /// it can be set to the same value as task.
        #[serde(borrow)]
        id: Cow<'t, str>,
        /// The number of bytes fetched by a previous run.
        bytes_cached: u64,
        /// The number of bytes already fetched.
        bytes: u64,
        /// Total number of bytes. If zero, then this should be considered "unspecified".
        bytes_total: u64,
        /// The number of steps fetched by a previous run.
        steps_cached: u64,
        /// The initial position of progress.
        steps: u64,
        /// The total number of steps (e.g. container image layers, RPMs)
        steps_total: u64,
        /// The currently running subtasks.
        subtasks: Vec<SubTaskBytes<'t>>,
    },
    /// An incremental update with discrete steps
    ProgressSteps {
        /// The version of the progress event format.
        #[serde(borrow)]
        api_version: Cow<'t, str>,
        /// A machine readable type (e.g., pulling) for the task (used for i18n
        /// and UI customization).
        #[serde(borrow)]
        task: Cow<'t, str>,
        /// A human readable description of the task if i18n is not available.
        #[serde(borrow)]
        description: Cow<'t, str>,
        /// A human and machine readable unique identifier for the task
        /// (e.g., the image name). For tasks that only happen once,
        /// it can be set to the same value as task.
        #[serde(borrow)]
        id: Cow<'t, str>,
        /// The number of steps fetched by a previous run.
        steps_cached: u64,
        /// The initial position of progress.
        steps: u64,
        /// The total number of steps (e.g. container image layers, RPMs)
        steps_total: u64,
        /// The currently running subtasks.
        subtasks: Vec<SubTaskStep<'t>>,
    },
}

#[derive(Debug)]
struct ProgressWriterInner {
    last_write: Option<std::time::Instant>,
    fd: BufWriter<Sender>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProgressWriter {
    inner: Arc<Mutex<Option<ProgressWriterInner>>>,
}

impl TryFrom<OwnedFd> for ProgressWriter {
    type Error = anyhow::Error;

    fn try_from(value: OwnedFd) -> Result<Self> {
        let value = Sender::from_owned_fd(value)?;
        Ok(Self::from(value))
    }
}

impl From<Sender> for ProgressWriter {
    fn from(value: Sender) -> Self {
        let inner = ProgressWriterInner {
            last_write: None,
            fd: BufWriter::new(value),
        };
        Self {
            inner: Arc::new(Some(inner).into()),
        }
    }
}

impl ProgressWriter {
    /// Given a raw file descriptor, create an instance of a json-lines writer.
    #[allow(unsafe_code)]
    #[context("Creating progress writer")]
    pub(crate) fn from_raw_fd(fd: RawFd) -> Result<Self> {
        unsafe { OwnedFd::from_raw_fd(fd) }.try_into()
    }

    /// Serialize the target object to JSON as a single line
    pub(crate) async fn send_impl<T: Serialize>(&self, v: T, required: bool) -> Result<()> {
        let mut guard = self.inner.lock().await;
        // Check if we have an inner value; if not, nothing to do.
        let Some(inner) = guard.as_mut() else {
            return Ok(());
        };

        // For messages that can be dropped, if we already sent an update within this cycle, discard this one.
        // TODO: Also consider querying the pipe buffer and also dropping if we can't do this write.
        let now = Instant::now();
        if !required {
            const REFRESH_MS: u32 = 1000 / REFRESH_HZ as u32;
            if let Some(elapsed) = inner.last_write.map(|w| now.duration_since(w)) {
                if elapsed.as_millis() < REFRESH_MS.into() {
                    return Ok(());
                }
            }
        }

        // SAFETY: Propagating panics from the mutex here is intentional
        // serde is guaranteed not to output newlines here
        let buf = serde_json::to_vec(&v)?;
        inner.fd.write_all(&buf).await?;
        // We always end in a newline
        inner.fd.write_all(b"\n").await?;
        // And flush to ensure the remote side sees updates immediately
        inner.fd.flush().await?;
        // Update the last write time
        inner.last_write = Some(now);
        Ok(())
    }

    /// Send an event.
    pub(crate) async fn send<T: Serialize>(&self, v: T) {
        if let Err(e) = self.send_impl(v, true).await {
            eprintln!("Failed to write to jsonl: {}", e);
            // Stop writing to fd but let process continue
            // SAFETY: Propagating panics from the mutex here is intentional
            let _ = self.inner.lock().await.take();
        }
    }

    /// Send an event that can be dropped.
    pub(crate) async fn send_lossy<T: Serialize>(&self, v: T) {
        if let Err(e) = self.send_impl(v, false).await {
            eprintln!("Failed to write to jsonl: {}", e);
            // Stop writing to fd but let process continue
            // SAFETY: Propagating panics from the mutex here is intentional
            let _ = self.inner.lock().await.take();
        }
    }

    /// Flush remaining data and return the underlying file.
    #[allow(dead_code)]
    pub(crate) async fn into_inner(self) -> Result<Option<Sender>> {
        // SAFETY: Propagating panics from the mutex here is intentional
        let mut mutex = self.inner.lock().await;
        if let Some(inner) = mutex.take() {
            Ok(Some(inner.fd.into_inner()))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod test {
    use serde::Deserialize;
    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::*;

    #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
    struct S {
        s: String,
        v: u32,
    }

    #[tokio::test]
    async fn test_jsonl() -> Result<()> {
        let testvalues = [
            S {
                s: "foo".into(),
                v: 42,
            },
            S {
                // Test with an embedded newline to sanity check that serde doesn't write it literally
                s: "foo\nbar".into(),
                v: 0,
            },
        ];
        let (send, recv) = tokio::net::unix::pipe::pipe()?;
        let testvalues_sender = &testvalues;
        let sender = async move {
            let w = ProgressWriter::try_from(send)?;
            for value in testvalues_sender {
                w.send(value).await;
            }
            anyhow::Ok(())
        };
        let testvalues = &testvalues;
        let receiver = async move {
            let tf = BufReader::new(recv);
            let mut expected = testvalues.iter();
            let mut lines = tf.lines();
            while let Some(line) = lines.next_line().await? {
                let found: S = serde_json::from_str(&line)?;
                let expected = expected.next().unwrap();
                assert_eq!(&found, expected);
            }
            anyhow::Ok(())
        };
        tokio::try_join!(sender, receiver)?;
        Ok(())
    }
}
