//! Internal API to interact with Open Container Images; mostly
//! oriented towards generating images.

use anyhow::{anyhow, Result};
use flate2::write::GzEncoder;
use fn_error_context::context;
use openat_ext::*;
use openssl::hash::{Hasher, MessageDigest};
use phf::phf_map;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    io::prelude::*,
};

/// Map the value from `uname -m` to the Go architecture.
/// TODO find a more canonical home for this.
static MACHINE_TO_OCI: phf::Map<&str, &str> = phf_map! {
    "x86_64" => "amd64",
    "aarch64" => "arm64",
};

// OCI types, see https://github.com/opencontainers/image-spec/blob/master/media-types.md
pub(crate) const OCI_TYPE_CONFIG_JSON: &str = "application/vnd.oci.image.config.v1+json";
pub(crate) const OCI_TYPE_MANIFEST_JSON: &str = "application/vnd.oci.image.manifest.v1+json";
pub(crate) const OCI_TYPE_LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
#[allow(dead_code)]
pub(crate) const IMAGE_LAYER_GZIP_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
pub(crate) const DOCKER_TYPE_LAYER: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// Path inside an OCI directory to the blobs
const BLOBDIR: &str = "blobs/sha256";

fn default_schema_version() -> u32 {
    2
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexPlatform {
    pub architecture: String,
    pub os: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexManifest {
    pub media_type: String,
    pub digest: String,
    pub size: u64,

    pub platform: Option<IndexPlatform>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Index {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    pub manifests: Vec<IndexManifest>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManifestLayer {
    pub media_type: String,
    pub digest: String,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Manifest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    pub layers: Vec<ManifestLayer>,
    pub annotations: Option<BTreeMap<String, String>>,
}

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
}

/// Completed layer metadata
#[derive(Debug)]
pub(crate) struct Layer {
    pub(crate) blob: Blob,
    pub(crate) uncompressed_sha256: String,
}

/// Create an OCI blob.
pub(crate) struct BlobWriter<'a> {
    pub(crate) hash: Hasher,
    pub(crate) target: Option<FileWriter<'a>>,
    size: u64,
}

/// Create an OCI layer (also a blob).
pub(crate) struct LayerWriter<'a> {
    bw: BlobWriter<'a>,
    uncompressed_hash: Hasher,
    compressor: GzEncoder<Vec<u8>>,
}

pub(crate) struct OciWriter<'a> {
    pub(crate) dir: &'a openat::Dir,

    config_annotations: HashMap<String, String>,
    manifest_annotations: HashMap<String, String>,

    root_layer: Option<Layer>,
}

/// Write a serializable data (JSON) as an OCI blob
#[context("Writing json blob")]
fn write_json_blob<S: serde::Serialize>(ocidir: &openat::Dir, v: &S) -> Result<Blob> {
    let mut w = BlobWriter::new(ocidir)?;
    {
        cjson::to_writer(&mut w, v).map_err(|e| anyhow!("{:?}", e))?;
    }

    w.complete()
}

impl<'a> OciWriter<'a> {
    pub(crate) fn new(dir: &'a openat::Dir) -> Result<Self> {
        dir.ensure_dir_all(BLOBDIR, 0o755)?;
        dir.write_file_contents("oci-layout", 0o644, r#"{"imageLayoutVersion":"1.0.0"}"#)?;

        Ok(Self {
            dir,
            config_annotations: Default::default(),
            manifest_annotations: Default::default(),
            root_layer: None,
        })
    }

    pub(crate) fn set_root_layer(&mut self, layer: Layer) {
        assert!(self.root_layer.replace(layer).is_none())
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
    pub(crate) fn complete(&mut self) -> Result<()> {
        let utsname = nix::sys::utsname::uname();
        let arch = MACHINE_TO_OCI[utsname.machine()];

        let rootfs_blob = self.root_layer.as_ref().unwrap();
        let root_layer_id = format!("sha256:{}", rootfs_blob.uncompressed_sha256);

        let config = serde_json::json!({
            "architecture": arch,
            "os": "linux",
            "config": {
                "Labels": self.config_annotations,
            },
            "rootfs": {
                "type": "layers",
                "diff_ids": [ root_layer_id ],
            },
            "history": [
                {
                    "commit": "created by ostree-container",
                }
            ]
        });
        let config_blob = write_json_blob(self.dir, &config)?;

        let manifest_data = serde_json::json!({
            "schemaVersion": default_schema_version(),
            "config": {
                "mediaType": OCI_TYPE_CONFIG_JSON,
                "size": config_blob.size,
                "digest": config_blob.digest_id(),
            },
            "layers": [
                { "mediaType": OCI_TYPE_LAYER,
                  "size": rootfs_blob.blob.size,
                  "digest":  rootfs_blob.blob.digest_id(),
                }
            ],
            "annotations": self.manifest_annotations,
        });
        let manifest_blob = write_json_blob(self.dir, &manifest_data)?;

        let index_data = serde_json::json!({
            "schemaVersion": default_schema_version(),
            "manifests": [
                {
                    "mediaType": OCI_TYPE_MANIFEST_JSON,
                    "digest": manifest_blob.digest_id(),
                    "size": manifest_blob.size,
                    "platform": {
                        "architecture": arch,
                        "os": "linux"
                    }
                }
            ]
        });
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

impl<'a> LayerWriter<'a> {
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

impl<'a> std::io::Write for LayerWriter<'a> {
    fn write(&mut self, srcbuf: &[u8]) -> std::io::Result<usize> {
        self.compressor.get_mut().clear();
        self.compressor.write_all(srcbuf).unwrap();
        let compressed_buf = self.compressor.get_mut().as_slice();
        self.bw.write_all(&compressed_buf)?;
        Ok(srcbuf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
