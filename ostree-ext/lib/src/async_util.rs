use anyhow::Result;
use futures::prelude::*;
use std::io::prelude::*;
use tokio::io::AsyncRead;

/// Bridge from AsyncRead to Read.
///
/// This creates a pipe and a "driver" future (which could be spawned or not).
pub(crate) fn copy_async_read_to_sync_pipe<S: AsyncRead + Unpin + Send + 'static>(
    s: S,
) -> Result<(impl Read, impl Future<Output = Result<()>>)> {
    let (pipein, mut pipeout) = os_pipe::pipe()?;

    let copier = async move {
        let mut input = tokio_util::io::ReaderStream::new(s).boxed();
        while let Some(buf) = input.next().await {
            let buf = buf?;
            // TODO blocking executor
            // Note broken pipe is OK, just means the caller stopped reading
            pipeout.write_all(&buf).or_else(|e| match e.kind() {
                std::io::ErrorKind::BrokenPipe => Ok(()),
                _ => Err(e),
            })?;
        }
        Ok::<_, anyhow::Error>(())
    };

    Ok((pipein, copier))
}
