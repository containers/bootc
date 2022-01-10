//! Internal API to interact with Open Container Images; mostly
//! oriented towards generating images.

use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
use flate2::write::GzEncoder;
use fn_error_context::context;
use oci_image::MediaType;
use oci_spec::image as oci_image;
use once_cell::sync::OnceCell;
use openat_ext::*;
use openssl::hash::{Hasher, MessageDigest};
use phf::phf_map;
use std::collections::HashMap;
use std::io::prelude::*;
use std::path::Path;
use std::rc::Rc;

/// Map the value from `uname -m` to the Go architecture.
/// TODO find a more canonical home for this.
static MACHINE_TO_OCI: phf::Map<&str, &str> = phf_map! {
    "x86_64" => "amd64",
    "aarch64" => "arm64",
};

static THIS_OCI_ARCH: Lazy<oci_image::Arch> = Lazy::new(|| {
    let machine = rustix::process::uname().machine();
    let arch = MACHINE_TO_OCI.get(machine).unwrap_or(&machine);
    oci_image::Arch::from(*arch)
});

/// Path inside an OCI directory to the blobs
const BLOBDIR: &str = "blobs/sha256";

/// Completed blob metadata
#[derive(Debug)]
pub(crate) struct Blob {
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

impl Blob {
    pub(crate) fn digest_id(&self) -> String {
        format!("sha256:{}", self.sha256)
    }

    pub(crate) fn descriptor(&self) -> oci_image::DescriptorBuilder {
        oci_image::DescriptorBuilder::default()
            .digest(self.digest_id())
            .size(self.size as i64)
    }
}

/// Completed layer metadata
#[derive(Debug)]
pub(crate) struct Layer {
    pub(crate) blob: Blob,
    pub(crate) uncompressed_sha256: String,
}

impl Layer {
    pub(crate) fn descriptor(&self) -> oci_image::DescriptorBuilder {
        self.blob.descriptor()
    }
}

/// Create an OCI blob.
pub(crate) struct BlobWriter<'a> {
    pub(crate) hash: Hasher,
    pub(crate) target: Option<FileWriter<'a>>,
    size: u64,
}

/// Create an OCI layer (also a blob).
pub(crate) struct RawLayerWriter<'a> {
    bw: BlobWriter<'a>,
    uncompressed_hash: Hasher,
    compressor: GzEncoder<Vec<u8>>,
}

pub(crate) struct OciDir {
    pub(crate) dir: Rc<openat::Dir>,
}

/// Write a serializable data (JSON) as an OCI blob
#[context("Writing json blob")]
pub(crate) fn write_json_blob<S: serde::Serialize>(
    ocidir: &openat::Dir,
    v: &S,
    media_type: oci_image::MediaType,
) -> Result<oci_image::DescriptorBuilder> {
    let mut w = BlobWriter::new(ocidir)?;
    cjson::to_writer(&mut w, v).map_err(|e| anyhow!("{:?}", e))?;
    let blob = w.complete()?;
    Ok(blob.descriptor().media_type(media_type))
}

fn deserialize_json_path<T: serde::de::DeserializeOwned + Send + 'static>(
    d: &openat::Dir,
    p: impl AsRef<Path>,
) -> Result<T> {
    let p = p.as_ref();
    let ctx = || format!("Parsing {:?}", p);
    let f = std::io::BufReader::new(d.open_file(p).with_context(ctx)?);
    serde_json::from_reader(f).with_context(ctx)
}

// Parse a filename from a string; this will ignore any directory components, and error out on `/` and `..` for example.
fn parse_one_filename(s: &str) -> Result<&str> {
    Utf8Path::new(s)
        .file_name()
        .ok_or_else(|| anyhow!("Invalid filename {}", s))
}

// Sadly the builder bits in the OCI spec don't offer mutable access to fields
// https://github.com/containers/oci-spec-rs/issues/86
fn vec_clone_append<T: Clone>(s: &[T], i: T) -> Vec<T> {
    s.iter().cloned().chain(std::iter::once(i)).collect()
}

/// Create a dummy config descriptor.
/// Our API right now always mutates a manifest, which means we need
/// a "valid" manifest, which requires a "valid" config descriptor.
/// This digest should never actually be used for anything.
fn empty_config_descriptor() -> oci_image::Descriptor {
    oci_image::DescriptorBuilder::default()
        .media_type(MediaType::ImageConfig)
        .size(7023)
        .digest("sha256:a5b2b2c507a0944348e0303114d8d93aaaa081732b86451d9bce1f432a537bc7")
        .build()
        .unwrap()
}

/// Generate a "valid" empty manifest.  See above.
pub(crate) fn new_empty_manifest() -> oci_image::ImageManifestBuilder {
    oci_image::ImageManifestBuilder::default()
        .schema_version(oci_image::SCHEMA_VERSION)
        .config(empty_config_descriptor())
        .layers(Vec::new())
}

/// Generate an image configuration targeting Linux for this architecture.
pub(crate) fn new_config() -> oci_image::ImageConfigurationBuilder {
    oci_image::ImageConfigurationBuilder::default()
        .architecture(THIS_OCI_ARCH.clone())
        .os(oci_image::Os::Linux)
}

/// Return a Platform object for Linux for this architecture.
pub(crate) fn this_platform() -> oci_image::Platform {
    oci_image::PlatformBuilder::default()
        .os(oci_image::Os::Linux)
        .architecture(THIS_OCI_ARCH.clone())
        .build()
        .unwrap()
}

impl OciDir {
    /// Create a new, empty OCI directory at the target path, which should be empty.
    pub(crate) fn create(dir: impl Into<Rc<openat::Dir>>) -> Result<Self> {
        let dir = dir.into();
        dir.ensure_dir_all(BLOBDIR, 0o755)?;
        dir.write_file_contents("oci-layout", 0o644, r#"{"imageLayoutVersion":"1.0.0"}"#)?;
        Self::open(dir)
    }

    #[allow(dead_code)]
    /// Clone an OCI directory, using reflinks for blobs.
    pub(crate) fn clone_to(&self, destdir: &openat::Dir, p: impl AsRef<Path>) -> Result<Self> {
        let p = p.as_ref();
        destdir.ensure_dir(p, 0o755)?;
        let cloned = Self::create(destdir.sub_dir(p)?)?;
        for blob in self.dir.list_dir(BLOBDIR)? {
            let blob = blob?;
            let path = Path::new(BLOBDIR).join(blob.file_name());
            self.dir.copy_file_at(&path, destdir, &path)?;
        }
        Ok(cloned)
    }

    /// Open an existing OCI directory.
    pub(crate) fn open(dir: impl Into<Rc<openat::Dir>>) -> Result<Self> {
        Ok(Self { dir: dir.into() })
    }

    /// Create a writer for a new blob (expected to be a tar stream)
    pub(crate) fn create_raw_layer(
        &self,
        c: Option<flate2::Compression>,
    ) -> Result<RawLayerWriter> {
        RawLayerWriter::new(&self.dir, c)
    }

    #[allow(dead_code)]
    /// Create a tar output stream, backed by a blob
    pub(crate) fn create_layer(
        &self,
        c: Option<flate2::Compression>,
    ) -> Result<tar::Builder<RawLayerWriter>> {
        Ok(tar::Builder::new(self.create_raw_layer(c)?))
    }

    /// Add a layer to the top of the image stack.  The firsh pushed layer becomes the root.
    #[allow(dead_code)]
    pub(crate) fn push_layer(
        &self,
        manifest: &mut oci_image::ImageManifest,
        config: &mut oci_image::ImageConfiguration,
        layer: Layer,
        description: &str,
    ) {
        let annotations: Option<HashMap<String, String>> = None;
        self.push_layer_annotated(manifest, config, layer, annotations, description);
    }

    /// Add a layer to the top of the image stack with optional annotations.
    ///
    /// This is otherwise equivalent to [`Self::push_layer`].
    pub(crate) fn push_layer_annotated(
        &self,
        manifest: &mut oci_image::ImageManifest,
        config: &mut oci_image::ImageConfiguration,
        layer: Layer,
        annotations: Option<impl Into<HashMap<String, String>>>,
        description: &str,
    ) {
        let mut builder = layer.descriptor().media_type(MediaType::ImageLayerGzip);
        if let Some(annotations) = annotations {
            builder = builder.annotations(annotations);
        }
        let blobdesc = builder.build().unwrap();
        manifest.set_layers(vec_clone_append(manifest.layers(), blobdesc));
        let mut rootfs = config.rootfs().clone();
        rootfs.set_diff_ids(vec_clone_append(
            rootfs.diff_ids(),
            format!("sha256:{}", layer.uncompressed_sha256),
        ));
        config.set_rootfs(rootfs);
        let h = oci_image::HistoryBuilder::default()
            .created_by(description.to_string())
            .build()
            .unwrap();
        config.set_history(vec_clone_append(config.history(), h));
    }

    /// Read a JSON blob.
    pub(crate) fn read_json_blob<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        desc: &oci_spec::image::Descriptor,
    ) -> Result<T> {
        let (alg, hash) = desc
            .digest()
            .split_once(':')
            .ok_or_else(|| anyhow!("Invalid digest {}", desc.digest()))?;
        let alg = parse_one_filename(alg)?;
        if alg != "sha256" {
            anyhow::bail!("Unsupported digest algorithm {}", desc.digest());
        }
        let hash = parse_one_filename(hash)?;
        deserialize_json_path(&self.dir, Path::new(BLOBDIR).join(hash))
    }

    /// Write a configuration blob.
    pub(crate) fn write_config(
        &self,
        config: oci_image::ImageConfiguration,
    ) -> Result<oci_image::Descriptor> {
        Ok(write_json_blob(&self.dir, &config, MediaType::ImageConfig)?
            .build()
            .unwrap())
    }

    /// Write a manifest as a blob, and replace the index with a reference to it.
    pub(crate) fn write_manifest(
        &self,
        manifest: oci_image::ImageManifest,
        platform: oci_image::Platform,
    ) -> Result<()> {
        let manifest = write_json_blob(&self.dir, &manifest, MediaType::ImageManifest)?
            .platform(platform)
            .build()
            .unwrap();

        let index_data = oci_image::ImageIndexBuilder::default()
            .schema_version(oci_image::SCHEMA_VERSION)
            .manifests(vec![manifest])
            .build()
            .unwrap();
        self.dir
            .write_file_with("index.json", 0o644, |w| -> Result<()> {
                cjson::to_writer(w, &index_data).map_err(|e| anyhow::anyhow!("{:?}", e))?;
                Ok(())
            })?;
        Ok(())
    }

    /// If this OCI directory has a single manifest, return it.  Otherwise, an error is returned.
    pub(crate) fn read_manifest(&self) -> Result<oci_image::ImageManifest> {
        let idx: oci_image::ImageIndex = deserialize_json_path(&self.dir, "index.json")?;
        let desc = match idx.manifests().as_slice() {
            [] => anyhow::bail!("No manifests found"),
            [desc] => desc,
            manifests => anyhow::bail!("Expected exactly 1 manifest, found {}", manifests.len()),
        };
        self.read_json_blob(desc)
    }
}

impl<'a> BlobWriter<'a> {
    #[context("Creating blob writer")]
    fn new(ocidir: &'a openat::Dir) -> Result<Self> {
        Ok(Self {
            hash: Hasher::new(MessageDigest::sha256())?,
            // FIXME add ability to choose filename after completion
            target: Some(ocidir.new_file_writer(0o644)?),
            size: 0,
        })
    }

    #[context("Completing blob")]
    /// Finish writing this blob object.
    pub(crate) fn complete(mut self) -> Result<Blob> {
        let sha256 = hex::encode(self.hash.finish()?);
        let target = &format!("{}/{}", BLOBDIR, sha256);
        self.target.take().unwrap().complete(target)?;
        Ok(Blob {
            sha256,
            size: self.size,
        })
    }
}

impl<'a> std::io::Write for BlobWriter<'a> {
    fn write(&mut self, srcbuf: &[u8]) -> std::io::Result<usize> {
        self.hash.update(srcbuf)?;
        self.target.as_mut().unwrap().writer.write_all(srcbuf)?;
        self.size += srcbuf.len() as u64;
        Ok(srcbuf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> RawLayerWriter<'a> {
    /// Create a writer for a gzip compressed layer blob.
    fn new(ocidir: &'a openat::Dir, c: Option<flate2::Compression>) -> Result<Self> {
        let bw = BlobWriter::new(ocidir)?;
        Ok(Self {
            bw,
            uncompressed_hash: Hasher::new(MessageDigest::sha256())?,
            compressor: GzEncoder::new(Vec::with_capacity(8192), c.unwrap_or_default()),
        })
    }

    #[context("Completing layer")]
    /// Consume this writer, flushing buffered data and put the blob in place.
    pub(crate) fn complete(mut self) -> Result<Layer> {
        self.compressor.get_mut().clear();
        let buf = self.compressor.finish()?;
        self.bw.write_all(&buf)?;
        let blob = self.bw.complete()?;
        let uncompressed_sha256 = hex::encode(self.uncompressed_hash.finish()?);
        Ok(Layer {
            blob,
            uncompressed_sha256,
        })
    }
}

impl<'a> std::io::Write for RawLayerWriter<'a> {
    fn write(&mut self, srcbuf: &[u8]) -> std::io::Result<usize> {
        self.compressor.get_mut().clear();
        self.compressor.write_all(srcbuf).unwrap();
        self.uncompressed_hash.update(srcbuf)?;
        let compressed_buf = self.compressor.get_mut().as_slice();
        self.bw.write_all(compressed_buf)?;
        Ok(srcbuf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.bw.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST_DERIVE: &str = r#"{
        "schemaVersion": 2,
        "config": {
          "mediaType": "application/vnd.oci.image.config.v1+json",
          "digest": "sha256:54977ab597b345c2238ba28fe18aad751e5c59dc38b9393f6f349255f0daa7fc",
          "size": 754
        },
        "layers": [
          {
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": "sha256:ee02768e65e6fb2bb7058282338896282910f3560de3e0d6cd9b1d5985e8360d",
            "size": 5462
          },
          {
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": "sha256:d203cef7e598fa167cb9e8b703f9f20f746397eca49b51491da158d64968b429",
            "size": 214
          }
        ],
        "annotations": {
          "ostree.commit": "3cb6170b6945065c2475bc16d7bebcc84f96b4c677811a6751e479b89f8c3770",
          "ostree.version": "42.0"
        }
      }
    "#;

    #[test]
    fn manifest() -> Result<()> {
        let m: oci_image::ImageManifest = serde_json::from_str(MANIFEST_DERIVE)?;
        assert_eq!(
            m.layers()[0].digest().as_str(),
            "sha256:ee02768e65e6fb2bb7058282338896282910f3560de3e0d6cd9b1d5985e8360d"
        );
        Ok(())
    }

    #[test]
    fn test_build() -> Result<()> {
        let td = tempfile::tempdir()?;
        let td = openat::Dir::open(td.path())?;
        let w = OciDir::create(td)?;
        let mut layerw = w.create_raw_layer(None)?;
        layerw.write_all(b"pretend this is a tarball")?;
        let root_layer = layerw.complete()?;
        assert_eq!(
            root_layer.uncompressed_sha256,
            "349438e5faf763e8875b43de4d7101540ef4d865190336c2cc549a11f33f8d7c"
        );
        let mut manifest = new_empty_manifest().build().unwrap();
        let mut config = oci_image::ImageConfigurationBuilder::default()
            .build()
            .unwrap();
        w.push_layer(&mut manifest, &mut config, root_layer, "root");
        let config = w.write_config(config)?;
        manifest.set_config(config);
        w.write_manifest(manifest, this_platform())?;
        Ok(())
    }
}
