use std::io::prelude::*;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncReadExt};

struct ReadBridge {
    reader: Pin<Box<dyn AsyncRead + Send + Unpin + 'static>>,
    rt: tokio::runtime::Handle,
}

impl Read for ReadBridge {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut reader = self.reader.as_mut();
        self.rt.block_on(async { reader.read(buf).await })
    }
}

/// Bridge from AsyncRead to Read.
pub(crate) fn async_read_to_sync<S: AsyncRead + Unpin + Send + 'static>(
    reader: S,
) -> impl Read + Send + Unpin + 'static {
    let rt = tokio::runtime::Handle::current();
    let reader = Box::pin(reader);
    ReadBridge { reader, rt }
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
        let mut r = async_read_to_sync(r);
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
