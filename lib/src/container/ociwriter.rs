//! Internal API to interact with Open Container Images; mostly
//! oriented towards generating images.

use anyhow::{anyhow, Result};
use flate2::write::GzEncoder;
use fn_error_context::context;
use oci_image::{Descriptor, MediaType};
use oci_spec::image as oci_image;
use openat_ext::*;
use openssl::hash::{Hasher, MessageDigest};
use phf::phf_map;
use std::collections::HashMap;
use std::io::prelude::*;

/// Map the value from `uname -m` to the Go architecture.
/// TODO find a more canonical home for this.
static MACHINE_TO_OCI: phf::Map<&str, &str> = phf_map! {
    "x86_64" => "amd64",
    "aarch64" => "arm64",
};

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

pub(crate) struct OciWriter<'a> {
    pub(crate) dir: &'a openat::Dir,

    config_annotations: HashMap<String, String>,
    manifest_annotations: HashMap<String, String>,

    cmd: Option<Vec<String>>,

    layers: Vec<Layer>,
}

/// Write a serializable data (JSON) as an OCI blob
#[context("Writing json blob")]
fn write_json_blob<S: serde::Serialize>(
    ocidir: &openat::Dir,
    v: &S,
    media_type: oci_image::MediaType,
) -> Result<oci_image::DescriptorBuilder> {
    let mut w = BlobWriter::new(ocidir)?;
    cjson::to_writer(&mut w, v).map_err(|e| anyhow!("{:?}", e))?;
    let blob = w.complete()?;
    Ok(blob.descriptor().media_type(media_type))
}

impl<'a> OciWriter<'a> {
    pub(crate) fn new(dir: &'a openat::Dir) -> Result<Self> {
        dir.ensure_dir_all(BLOBDIR, 0o755)?;
        dir.write_file_contents("oci-layout", 0o644, r#"{"imageLayoutVersion":"1.0.0"}"#)?;

        Ok(Self {
            dir,
            config_annotations: Default::default(),
            manifest_annotations: Default::default(),
            layers: Vec::new(),
            cmd: None,
        })
    }

    /// Create a writer for a new blob (expected to be a tar stream)
    pub(crate) fn create_raw_layer(
        &self,
        c: Option<flate2::Compression>,
    ) -> Result<RawLayerWriter> {
        RawLayerWriter::new(self.dir, c)
    }

    #[allow(dead_code)]
    /// Create a tar output stream, backed by a blob
    pub(crate) fn create_layer(
        &self,
        c: Option<flate2::Compression>,
    ) -> Result<tar::Builder<RawLayerWriter>> {
        Ok(tar::Builder::new(self.create_raw_layer(c)?))
    }

    #[allow(dead_code)]
    /// Finish all I/O for a layer writer, and add it to the layers in the image.
    pub(crate) fn finish_and_push_layer(&mut self, w: RawLayerWriter) -> Result<()> {
        let w = w.complete()?;
        self.push_layer(w);
        Ok(())
    }

    /// Add a layer to the top of the image stack.  The firsh pushed layer becomes the root.
    pub(crate) fn push_layer(&mut self, layer: Layer) {
        self.layers.push(layer)
    }

    pub(crate) fn set_cmd(&mut self, e: &[&str]) {
        self.cmd = Some(e.iter().map(|s| s.to_string()).collect());
    }

    pub(crate) fn add_manifest_annotation<K: AsRef<str>, V: AsRef<str>>(&mut self, k: K, v: V) {
        let k = k.as_ref();
        let v = v.as_ref();
        self.manifest_annotations
            .insert(k.to_string(), v.to_string());
    }

    pub(crate) fn add_config_annotation<K: AsRef<str>, V: AsRef<str>>(&mut self, k: K, v: V) {
        let k = k.as_ref();
        let v = v.as_ref();
        self.config_annotations.insert(k.to_string(), v.to_string());
    }

    #[context("Writing OCI")]
    pub(crate) fn complete(self) -> Result<()> {
        let utsname = nix::sys::utsname::uname();
        let machine = utsname.machine();
        let arch = MACHINE_TO_OCI.get(machine).unwrap_or(&machine);
        let arch = oci_image::Arch::from(*arch);

        if self.layers.is_empty() {
            return Err(anyhow!("No layers specified"));
        }

        let diffids: Vec<String> = self
            .layers
            .iter()
            .map(|l| format!("sha256:{}", l.uncompressed_sha256))
            .collect();
        let rootfs = oci_image::RootFsBuilder::default()
            .diff_ids(diffids)
            .build()
            .unwrap();

        let ctrconfig_builder = oci_image::ConfigBuilder::default().labels(self.config_annotations);
        let ctrconfig = if let Some(cmd) = self.cmd {
            ctrconfig_builder.cmd(cmd)
        } else {
            ctrconfig_builder
        }
        .build()
        .unwrap();
        let history = oci_image::HistoryBuilder::default()
            .created_by(format!(
                "created by {} {}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .unwrap();
        let config = oci_image::ImageConfigurationBuilder::default()
            .architecture(arch.clone())
            .os(oci_image::Os::Linux)
            .config(ctrconfig)
            .rootfs(rootfs)
            .history(vec![history])
            .build()
            .unwrap();
        let config_blob = write_json_blob(self.dir, &config, MediaType::ImageConfig)?;

        let layers: Vec<Descriptor> = self
            .layers
            .iter()
            .map(|layer| {
                layer
                    .descriptor()
                    .media_type(MediaType::ImageLayerGzip)
                    .build()
                    .unwrap()
            })
            .collect();
        let manifest_data = oci_image::ImageManifestBuilder::default()
            .schema_version(oci_image::SCHEMA_VERSION)
            .config(config_blob.build().unwrap())
            .layers(layers)
            .annotations(self.manifest_annotations)
            .build()
            .unwrap();
        let manifest = write_json_blob(self.dir, &manifest_data, MediaType::ImageManifest)?
            .platform(
                oci_image::PlatformBuilder::default()
                    .architecture(arch)
                    .os(oci_spec::image::Os::Linux)
                    .build()
                    .unwrap(),
            )
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
}

impl<'a> BlobWriter<'a> {
    #[context("Creating blob writer")]
    pub(crate) fn new(ocidir: &'a openat::Dir) -> Result<Self> {
        Ok(Self {
            hash: Hasher::new(MessageDigest::sha256())?,
            // FIXME add ability to choose filename after completion
            target: Some(ocidir.new_file_writer(0o644)?),
            size: 0,
        })
    }

    #[context("Completing blob")]
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
    pub(crate) fn new(ocidir: &'a openat::Dir, c: Option<flate2::Compression>) -> Result<Self> {
        let bw = BlobWriter::new(ocidir)?;
        Ok(Self {
            bw,
            uncompressed_hash: Hasher::new(MessageDigest::sha256())?,
            compressor: GzEncoder::new(Vec::with_capacity(8192), c.unwrap_or_default()),
        })
    }

    #[context("Completing layer")]
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
        let td = &openat::Dir::open(td.path())?;
        let mut w = OciWriter::new(td)?;
        let mut layerw = w.create_raw_layer(None)?;
        layerw.write_all(b"pretend this is a tarball")?;
        let root_layer = layerw.complete()?;
        assert_eq!(
            root_layer.uncompressed_sha256,
            "349438e5faf763e8875b43de4d7101540ef4d865190336c2cc549a11f33f8d7c"
        );
        w.push_layer(root_layer);
        w.complete()?;
        Ok(())
    }
}
