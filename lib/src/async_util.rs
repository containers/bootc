use std::io::prelude::*;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncReadExt};

/// A [`std::io::Read`] implementation backed by an asynchronous source.
pub(crate) struct ReadBridge<T> {
    reader: Pin<Box<T>>,
    rt: tokio::runtime::Handle,
}

impl<T: AsyncRead> Read for ReadBridge<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let reader = &mut self.reader;
        self.rt.block_on(async { reader.read(buf).await })
    }
}

impl<T: AsyncRead> ReadBridge<T> {
    /// Create a [`std::io::Read`] implementation backed by an asynchronous source.
    ///
    /// This is useful with e.g. [`tokio::task::spawn_blocking`].
    pub(crate) fn new(reader: T) -> Self {
        let reader = Box::pin(reader);
        let rt = tokio::runtime::Handle::current();
        ReadBridge { reader, rt }
    }
}

#[cfg(test)]
mod test {
    use std::convert::TryInto;

    use super::*;
    use anyhow::Result;

    async fn test_reader_len(
        r: impl AsyncRead + Unpin + Send + 'static,
        expected_len: usize,
    ) -> Result<()> {
        let mut r = ReadBridge::new(r);
        let res = tokio::task::spawn_blocking(move || {
            let mut buf = Vec::new();
            r.read_to_end(&mut buf)?;
            Ok::<_, anyhow::Error>(buf)
        })
        .await?;
        assert_eq!(res?.len(), expected_len);
        Ok(())
    }

    #[tokio::test]
    async fn test_async_read_to_sync() -> Result<()> {
        test_reader_len(tokio::io::empty(), 0).await?;
        let bash = tokio::fs::File::open("/usr/bin/sh").await?;
        let bash_len = bash.metadata().await?.len();
        test_reader_len(bash, bash_len.try_into().unwrap()).await?;
        Ok(())
    }
}
