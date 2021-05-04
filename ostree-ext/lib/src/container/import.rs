//! APIs for extracting OSTree commits from container images

use super::*;
use anyhow::{anyhow, Context};
use camino::Utf8Path;
use fn_error_context::context;
use futures::prelude::*;
use std::io::prelude::*;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::AsyncRead;
use tracing::{event, instrument, Level};

/// The result of an import operation
#[derive(Copy, Clone, Debug, Default)]
pub struct ImportProgress {
    /// Number of bytes downloaded (approximate)
    pub processed_bytes: u64,
}

type Progress = tokio::sync::watch::Sender<ImportProgress>;

/// A read wrapper that updates the download progress.
struct ProgressReader {
    reader: Box<dyn AsyncRead + Unpin + Send + 'static>,
    progress: Option<Progress>,
}

impl AsyncRead for ProgressReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let pinned = Pin::new(&mut self.reader);
        let len = buf.filled().len();
        match pinned.poll_read(cx, buf) {
            v @ std::task::Poll::Ready(Ok(_)) => {
                let success = if let Some(progress) = self.progress.as_ref() {
                    let state = {
                        let mut state = *progress.borrow();
                        let newlen = buf.filled().len();
                        debug_assert!(newlen >= len);
                        let read = (newlen - len) as u64;
                        state.processed_bytes += read;
                        state
                    };
                    // Ignore errors, if the caller disconnected from progress that's OK.
                    progress.send(state).is_ok()
                } else {
                    true
                };
                if !success {
                    let _ = self.progress.take();
                }
                v
            }
            o => o,
        }
    }
}

/// Download the manifest for a target image.
#[context("Fetching manifest")]
pub async fn fetch_manifest_info(imgref: &ImageReference) -> Result<OstreeContainerManifestInfo> {
    let (_, manifest_digest) = fetch_manifest(imgref).await?;
    // Sadly this seems to be lost when pushing to e.g. quay.io, which means we can't use it.
    //    let commit = manifest
    //        .annotations
    //        .as_ref()
    //        .map(|a| a.get(OSTREE_COMMIT_LABEL))
    //        .flatten()
    //        .ok_or_else(|| anyhow!("Missing annotation {}", OSTREE_COMMIT_LABEL))?;
    Ok(OstreeContainerManifestInfo { manifest_digest })
}

/// Download the manifest for a target image.
#[context("Fetching manifest")]
async fn fetch_manifest(imgref: &ImageReference) -> Result<(oci::Manifest, String)> {
    let mut proc = skopeo::new_cmd();
    proc.args(&["inspect", "--raw"]).arg(imgref.to_string());
    proc.stdout(Stdio::piped());
    let proc = skopeo::spawn(proc)?.wait_with_output().await?;
    if !proc.status.success() {
        let errbuf = String::from_utf8_lossy(&proc.stderr);
        return Err(anyhow!("skopeo inspect failed\n{}", errbuf));
    }
    let raw_manifest = proc.stdout;
    let digest = openssl::hash::hash(openssl::hash::MessageDigest::sha256(), &raw_manifest)?;
    let digest = format!("sha256:{}", hex::encode(digest.as_ref()));
    Ok((serde_json::from_slice(&raw_manifest)?, digest))
}

/// Read the contents of the first <checksum>.tar we find
pub async fn find_layer_tar(
    src: impl AsyncRead + Send + Unpin + 'static,
    blobid: &str,
) -> Result<(impl AsyncRead, impl Future<Output = Result<()>>)> {
    let (pipein, input_copydriver) = crate::async_util::copy_async_read_to_sync_pipe(src)?;
    let (tx_buf, rx_buf) = tokio::sync::mpsc::channel(2);
    let blob_symlink_target = format!("../{}.tar", blobid);
    let import = tokio::task::spawn_blocking(move || {
        let mut archive = tar::Archive::new(pipein);
        let mut buf = vec![0u8; 8192];
        for entry in archive.entries()? {
            let mut entry = entry.context("Reading entry")?;
            let path = entry.path()?;
            let path = &*path;
            let path = Utf8Path::from_path(path)
                .ok_or_else(|| anyhow!("Invalid non-utf8 path {:?}", path))?;
            let t = entry.header().entry_type();

            // We generally expect our layer to be first, but let's just skip anything
            // unexpected to be robust against changes in skopeo.
            if path.extension() != Some("tar") {
                continue;
            }

            event!(Level::DEBUG, "Found {}", path);

            match t {
                tar::EntryType::Symlink => {
                    if let Some(name) = path.file_name() {
                        if name == "layer.tar" {
                            let target = entry
                                .link_name()?
                                .ok_or_else(|| anyhow!("Invalid link {}", path))?;
                            let target = Utf8Path::from_path(&*target)
                                .ok_or_else(|| anyhow!("Invalid non-UTF8 path {:?}", target))?;
                            if target != blob_symlink_target {
                                return Err(anyhow!(
                                    "Found unexpected layer link {} -> {}",
                                    path,
                                    target
                                ));
                            }
                        }
                    }
                }
                tar::EntryType::Regular => loop {
                    let n = entry
                        .read(&mut buf[..])
                        .context("Reading tar file contents")?;
                    let done = 0 == n;
                    let r = Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(&buf[0..n]));
                    let receiver_closed = tx_buf.blocking_send(r).is_err();
                    if receiver_closed || done {
                        return Ok::<_, anyhow::Error>(());
                    }
                },
                _ => continue,
            }
        }
        Err(anyhow!("Failed to find layer {}", blob_symlink_target))
    })
    .map_err(anyhow::Error::msg);
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx_buf);
    let reader = tokio_util::io::StreamReader::new(stream);
    // Is there a better way to do this?
    let worker = async move {
        let (import, input_copydriver) = tokio::join!(import, input_copydriver);
        let _: () = import?.context("Import worker")?;
        let _: () = input_copydriver.context("Layer input copy driver failed")?;
        Ok::<_, anyhow::Error>(())
    };
    Ok((reader, worker))
}

/// Fetch a remote docker/OCI image and extract a specific uncompressed layer.
async fn fetch_layer<'s>(
    imgref: &ImageReference,
    blobid: &str,
    progress: Option<tokio::sync::watch::Sender<ImportProgress>>,
) -> Result<(
    impl AsyncRead + Unpin + Send,
    impl Future<Output = Result<()>>,
)> {
    let mut proc = skopeo::new_cmd();
    proc.stdout(Stdio::null());
    let tempdir = tempfile::Builder::new()
        .prefix("ostree-rs-ext")
        .tempdir_in("/var/tmp")?;
    let tempdir = Utf8Path::from_path(tempdir.path()).unwrap();
    let fifo = &tempdir.join("skopeo.pipe");
    nix::unistd::mkfifo(
        fifo.as_os_str(),
        nix::sys::stat::Mode::from_bits(0o600).unwrap(),
    )?;
    tracing::trace!("skopeo pull starting to {}", fifo);
    proc.arg("copy")
        .arg(imgref.to_string())
        .arg(format!("docker-archive:{}", fifo));
    let mut proc = skopeo::spawn(proc)?;
    let fifo_reader = ProgressReader {
        reader: Box::new(tokio::fs::File::open(fifo).await?),
        progress: progress,
    };
    let waiter = async move {
        let res = proc.wait().await?;
        if !res.success() {
            return Err(anyhow!("skopeo failed: {}", res));
        }
        Ok(())
    }
    .boxed();
    let (contents, worker) = find_layer_tar(fifo_reader, blobid).await?;
    let worker = async move {
        let (worker, waiter) = tokio::join!(worker, waiter);
        let _: () = worker.context("Layer worker failed")?;
        let _: () = waiter?;
        Ok::<_, anyhow::Error>(())
    };
    Ok((contents, worker))
}

/// The result of an import operation
#[derive(Debug)]
pub struct Import {
    /// The ostree commit that was imported
    pub ostree_commit: String,
    /// The image digest retrieved
    pub image_digest: String,
}

fn find_layer_blobid(manifest: &oci::Manifest) -> Result<String> {
    let layers: Vec<_> = manifest
        .layers
        .iter()
        .filter(|&layer| {
            matches!(
                layer.media_type.as_str(),
                super::oci::DOCKER_TYPE_LAYER | oci::OCI_TYPE_LAYER
            )
        })
        .collect();

    let n = layers.len();
    if let Some(layer) = layers.into_iter().next() {
        if n > 1 {
            Err(anyhow!("Expected 1 layer, found {}", n))
        } else {
            let digest = layer.digest.as_str();
            let hash = digest
                .strip_prefix("sha256:")
                .ok_or_else(|| anyhow!("Expected sha256: in digest: {}", digest))?;
            Ok(hash.into())
        }
    } else {
        Err(anyhow!("No layers found (orig: {})", manifest.layers.len()))
    }
}

/// Fetch a container image and import its embedded OSTree commit.
#[context("Importing {}", imgref)]
#[instrument(skip(repo, progress))]
pub async fn import(
    repo: &ostree::Repo,
    imgref: &ImageReference,
    progress: Option<tokio::sync::watch::Sender<ImportProgress>>,
) -> Result<Import> {
    let (manifest, image_digest) = fetch_manifest(imgref).await?;
    let manifest = &manifest;
    let layerid = find_layer_blobid(manifest)?;
    event!(Level::DEBUG, "target blob: {}", layerid);
    let (blob, worker) = fetch_layer(imgref, layerid.as_str(), progress).await?;
    let blob = tokio::io::BufReader::new(blob);
    let import = crate::tar::import_tar(&repo, blob);
    let (ostree_commit, worker) = tokio::join!(import, worker);
    let ostree_commit = ostree_commit?;
    let _: () = worker?;
    event!(Level::DEBUG, "created commit {}", ostree_commit);
    Ok(Import {
        ostree_commit,
        image_digest,
    })
}
