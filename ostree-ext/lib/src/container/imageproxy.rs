//! Run container-image-proxy as a subprocess.
//! This allows fetching a container image manifest and layers in a streaming fashion.
//! More information: <https://github.com/cgwalters/container-image-proxy>

use super::{oci, ImageReference, Result};
use crate::cmdext::CommandRedirectionExt;
use anyhow::Context;
use futures_util::{Future, FutureExt, TryFutureExt, TryStreamExt};
use hyper::body::HttpBody;
use hyper::client::conn::{Builder, SendRequest};
use hyper::{Body, Request, StatusCode};
use std::os::unix::prelude::AsRawFd;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncBufRead, AsyncReadExt};

// What we get from boxing a fallible tokio::spawn() closure.  Note the nested Result.
type JoinFuture<T> = Pin<Box<dyn Future<Output = Result<Result<T>>>>>;

/// Manage a child process proxy to fetch container images.
pub(crate) struct ImageProxy {
    proc: tokio::process::Child,
    request_sender: SendRequest<Body>,
    stderr: JoinFuture<String>,
    driver: JoinFuture<()>,
}

impl ImageProxy {
    /// Create an image proxy that fetches the target image.
    pub(crate) async fn new(imgref: &ImageReference) -> Result<Self> {
        // Communicate over an anonymous socketpair(2)
        let (mysock, childsock) = tokio::net::UnixStream::pair()?;
        let childsock = childsock.into_std()?;
        let mut c = std::process::Command::new("container-image-proxy");
        c.arg(&imgref.to_string());
        c.stdout(Stdio::null()).stderr(Stdio::piped());
        if let Some(port) = std::env::var_os("OSTREE_IMAGE_PROXY_PORT") {
            c.arg("--port");
            c.arg(port);
        } else {
            // Pass one half of the pair as fd 3 to the child
            let target_fd = 3;
            c.arg("--sockfd");
            c.arg(&format!("{}", target_fd));
            c.take_fd_n(childsock.as_raw_fd(), target_fd);
        }
        let mut c = tokio::process::Command::from(c);
        c.kill_on_drop(true);
        let mut proc = c.spawn().context("Failed to spawn container-image-proxy")?;
        // We've passed over the fd, close it.
        drop(childsock);

        // Safety: We passed `Stdio::piped()` above
        let mut child_stderr = proc.stderr.take().unwrap();

        // Connect via HTTP to the child
        let (request_sender, connection) = Builder::new().handshake::<_, Body>(mysock).await?;
        // Background driver that manages things like timeouts.
        let driver = tokio::spawn(connection.map_err(anyhow::Error::msg))
            .map_err(anyhow::Error::msg)
            .boxed();
        let stderr = tokio::spawn(async move {
            let mut buf = String::new();
            child_stderr.read_to_string(&mut buf).await?;
            Ok(buf)
        })
        .map_err(anyhow::Error::msg)
        .boxed();
        Ok(Self {
            proc,
            stderr,
            request_sender,
            driver,
        })
    }

    /// Fetch the manifest.
    /// https://github.com/opencontainers/image-spec/blob/main/manifest.md
    pub(crate) async fn fetch_manifest(&mut self) -> Result<(String, Vec<u8>)> {
        let req = Request::builder()
            .header("Host", "localhost")
            .method("GET")
            .uri("/manifest")
            .body(Body::from(""))?;
        let mut resp = self.request_sender.send_request(req).await?;
        if resp.status() != StatusCode::OK {
            return Err(anyhow::anyhow!("error from proxy: {}", resp.status()));
        }
        let hname = "Manifest-Digest";
        let digest = resp
            .headers()
            .get(hname)
            .ok_or_else(|| anyhow::anyhow!("Missing {} header", hname))?
            .to_str()
            .with_context(|| format!("Invalid {} header", hname))?
            .to_string();
        let mut ret = Vec::new();
        while let Some(chunk) = resp.body_mut().data().await {
            let chunk = chunk?;
            ret.extend_from_slice(&chunk);
        }
        Ok((digest, ret))
    }

    /// Fetch a blob identified by e.g. `sha256:<digest>`.
    /// https://github.com/opencontainers/image-spec/blob/main/descriptor.md
    /// Note that right now the proxy does verification of the digest:
    /// https://github.com/cgwalters/container-image-proxy/issues/1#issuecomment-926712009
    pub(crate) async fn fetch_blob(
        &mut self,
        digest: &str,
    ) -> Result<impl AsyncBufRead + Send + Unpin> {
        let uri = format!("/blobs/{}", digest);
        let req = Request::builder()
            .header("Host", "localhost")
            .method("GET")
            .uri(&uri)
            .body(Body::from(""))?;
        let resp = self.request_sender.send_request(req).await?;
        let status = resp.status();
        let body = TryStreamExt::map_err(resp.into_body(), |e| {
            std::io::Error::new(std::io::ErrorKind::Other, e)
        });
        let mut body = tokio_util::io::StreamReader::new(body);
        if status != StatusCode::OK {
            let mut s = String::new();
            let _: usize = body.read_to_string(&mut s).await?;
            return Err(anyhow::anyhow!("error from proxy: {}: {}", status, s));
        }
        Ok(body)
    }

    /// A wrapper for [`fetch_blob`] which fetches a layer and decompresses it.
    pub(crate) async fn fetch_layer_decompress(
        &mut self,
        layer: &oci::ManifestLayer,
    ) -> Result<Box<dyn AsyncBufRead + Send + Unpin>> {
        let blob = self.fetch_blob(layer.digest.as_str()).await?;
        Ok(layer.new_async_decompressor(blob)?)
    }

    /// Close the HTTP connection and wait for the child process to exit successfully.
    pub(crate) async fn finalize(mut self) -> Result<()> {
        // For now discard any errors from the connection
        drop(self.request_sender);
        let _r = self.driver.await??;
        let status = self.proc.wait().await?;
        if !status.success() {
            if let Some(stderr) = self.stderr.await.map(|v| v.ok()).ok().flatten() {
                anyhow::bail!("proxy failed: {}\n{}", status, stderr)
            } else {
                anyhow::bail!("proxy failed: {} (failed to fetch stderr)", status)
            }
        }
        Ok(())
    }
}
