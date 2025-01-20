use std::collections::HashMap;
#[cfg(feature = "install-to-disk")]
use std::env;
#[cfg(feature = "install-to-disk")]
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
#[cfg(feature = "install-to-disk")]
use camino::Utf8PathBuf;
use fn_error_context::context;
use regex::Regex;
use serde::Deserialize;

use bootc_utils::CommandRunExt;

#[derive(Debug, Deserialize)]
struct DevicesOutput {
    blockdevices: Vec<Device>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct Device {
    pub(crate) name: String,
    pub(crate) serial: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) partlabel: Option<String>,
    pub(crate) children: Option<Vec<Device>>,
    pub(crate) size: u64,
    #[serde(rename = "maj:min")]
    pub(crate) maj_min: Option<String>,
    // NOTE this one is not available on older util-linux, and
    // will also not exist for whole blockdevs (as opposed to partitions).
    pub(crate) start: Option<u64>,

    // Filesystem-related properties
    pub(crate) label: Option<String>,
    pub(crate) fstype: Option<String>,
    pub(crate) path: Option<String>,
}

impl Device {
    #[allow(dead_code)]
    // RHEL8's lsblk doesn't have PATH, so we do it
    pub(crate) fn path(&self) -> String {
        self.path.clone().unwrap_or(format!("/dev/{}", &self.name))
    }

    #[allow(dead_code)]
    pub(crate) fn has_children(&self) -> bool {
        self.children.as_ref().map_or(false, |v| !v.is_empty())
    }

    // The "start" parameter was only added in a version of util-linux that's only
    // in Fedora 40 as of this writing.
    fn backfill_start(&mut self) -> Result<()> {
        let Some(majmin) = self.maj_min.as_deref() else {
            // This shouldn't happen
            return Ok(());
        };
        let sysfs_start_path = format!("/sys/dev/block/{majmin}/start");
        if Utf8Path::new(&sysfs_start_path).try_exists()? {
            let start = std::fs::read_to_string(&sysfs_start_path)
                .with_context(|| format!("Reading {sysfs_start_path}"))?;
            tracing::debug!("backfilled start to {start}");
            self.start = Some(
                start
                    .trim()
                    .parse()
                    .context("Parsing sysfs start property")?,
            );
        }
        Ok(())
    }

    /// Older versions of util-linux may be missing some properties. Backfill them if they're missing.
    pub(crate) fn backfill_missing(&mut self) -> Result<()> {
        // Add new properties to backfill here
        self.backfill_start()?;
        // And recurse to child devices
        for child in self.children.iter_mut().flatten() {
            child.backfill_missing()?;
        }
        Ok(())
    }
}

#[context("Listing device {dev}")]
pub(crate) fn list_dev(dev: &Utf8Path) -> Result<Device> {
    let mut devs: DevicesOutput = Command::new("lsblk")
        .args(["-J", "-b", "-O"])
        .arg(dev)
        .log_debug()
        .run_and_parse_json()?;
    for dev in devs.blockdevices.iter_mut() {
        dev.backfill_missing()?;
    }
    devs.blockdevices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no device output from lsblk for {dev}"))
}

#[derive(Debug, Deserialize)]
struct SfDiskOutput {
    partitiontable: PartitionTable,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct Partition {
    pub(crate) node: String,
    pub(crate) start: u64,
    pub(crate) size: u64,
    #[serde(rename = "type")]
    pub(crate) parttype: String,
    pub(crate) uuid: Option<String>,
    pub(crate) name: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PartitionType {
    Dos,
    Gpt,
    Unknown(String),
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct PartitionTable {
    pub(crate) label: PartitionType,
    pub(crate) id: String,
    pub(crate) device: String,
    // We're not using these fields
    // pub(crate) unit: String,
    // pub(crate) firstlba: u64,
    // pub(crate) lastlba: u64,
    // pub(crate) sectorsize: u64,
    pub(crate) partitions: Vec<Partition>,
}

impl PartitionTable {
    /// Find the partition with the given device name
    #[allow(dead_code)]
    pub(crate) fn find<'a>(&'a self, devname: &str) -> Option<&'a Partition> {
        self.partitions.iter().find(|p| p.node.as_str() == devname)
    }

    pub(crate) fn path(&self) -> &Utf8Path {
        self.device.as_str().into()
    }

    // Find the partition with the given offset (starting at 1)
    #[allow(dead_code)]
    pub(crate) fn find_partno(&self, partno: u32) -> Result<&Partition> {
        let r = self
            .partitions
            .get(partno.checked_sub(1).expect("1 based partition offset") as usize)
            .ok_or_else(|| anyhow::anyhow!("Missing partition for index {partno}"))?;
        Ok(r)
    }
}

impl Partition {
    #[allow(dead_code)]
    pub(crate) fn path(&self) -> &Utf8Path {
        self.node.as_str().into()
    }
}

#[context("Listing partitions of {dev}")]
pub(crate) fn partitions_of(dev: &Utf8Path) -> Result<PartitionTable> {
    let o: SfDiskOutput = Command::new("sfdisk")
        .args(["-J", dev.as_str()])
        .run_and_parse_json()?;
    Ok(o.partitiontable)
}

#[cfg(feature = "install-to-disk")]
pub(crate) struct LoopbackDevice {
    pub(crate) dev: Option<Utf8PathBuf>,
}

#[cfg(feature = "install-to-disk")]
impl LoopbackDevice {
    // Create a new loopback block device targeting the provided file path.
    pub(crate) fn new(path: &Path) -> Result<Self> {
        let direct_io = match env::var("BOOTC_DIRECT_IO") {
            Ok(val) => {
                if val == "on" {
                    "on"
                } else {
                    "off"
                }
            }
            Err(_e) => "off",
        };

        let dev = Command::new("losetup")
            .args([
                "--show",
                format!("--direct-io={direct_io}").as_str(),
                "-P",
                "--find",
            ])
            .arg(path)
            .run_get_string()?;
        let dev = Utf8PathBuf::from(dev.trim());
        tracing::debug!("Allocated loopback {dev}");
        Ok(Self { dev: Some(dev) })
    }

    // Access the path to the loopback block device.
    pub(crate) fn path(&self) -> &Utf8Path {
        // SAFETY: The option cannot be destructured until we are dropped
        self.dev.as_deref().unwrap()
    }

    // Shared backend for our `close` and `drop` implementations.
    fn impl_close(&mut self) -> Result<()> {
        // SAFETY: This is the only place we take the option
        let Some(dev) = self.dev.take() else {
            tracing::trace!("loopback device already deallocated");
            return Ok(());
        };
        Command::new("losetup").args(["-d", dev.as_str()]).run()
    }

    /// Consume this device, unmounting it.
    pub(crate) fn close(mut self) -> Result<()> {
        self.impl_close()
    }
}

#[cfg(feature = "install-to-disk")]
impl Drop for LoopbackDevice {
    fn drop(&mut self) {
        // Best effort to unmount if we're dropped without invoking `close`
        let _ = self.impl_close();
    }
}

/// Parse key-value pairs from lsblk --pairs.
/// Newer versions of lsblk support JSON but the one in CentOS 7 doesn't.
fn split_lsblk_line(line: &str) -> HashMap<String, String> {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    let regex = REGEX.get_or_init(|| Regex::new(r#"([A-Z-_]+)="([^"]+)""#).unwrap());
    let mut fields: HashMap<String, String> = HashMap::new();
    for cap in regex.captures_iter(line) {
        fields.insert(cap[1].to_string(), cap[2].to_string());
    }
    fields
}

/// This is a bit fuzzy, but... this function will return every block device in the parent
/// hierarchy of `device` capable of containing other partitions. So e.g. parent devices of type
/// "part" doesn't match, but "disk" and "mpath" does.
pub(crate) fn find_parent_devices(device: &str) -> Result<Vec<String>> {
    let output = Command::new("lsblk")
        // Older lsblk, e.g. in CentOS 7.6, doesn't support PATH, but --paths option
        .arg("--pairs")
        .arg("--paths")
        .arg("--inverse")
        .arg("--output")
        .arg("NAME,TYPE")
        .arg(device)
        .run_get_string()?;
    let mut parents = Vec::new();
    // skip first line, which is the device itself
    for line in output.lines().skip(1) {
        let dev = split_lsblk_line(line);
        let name = dev
            .get("NAME")
            .with_context(|| format!("device in hierarchy of {device} missing NAME"))?;
        let kind = dev
            .get("TYPE")
            .with_context(|| format!("device in hierarchy of {device} missing TYPE"))?;
        if kind == "disk" || kind == "loop" {
            parents.push(name.clone());
        } else if kind == "mpath" {
            parents.push(name.clone());
            // we don't need to know what disks back the multipath
            break;
        }
    }
    Ok(parents)
}

/// Parse a string into mibibytes
#[cfg(feature = "install-to-disk")]
pub(crate) fn parse_size_mib(mut s: &str) -> Result<u64> {
    let suffixes = [
        ("MiB", 1u64),
        ("M", 1u64),
        ("GiB", 1024),
        ("G", 1024),
        ("TiB", 1024 * 1024),
        ("T", 1024 * 1024),
    ];
    let mut mul = 1u64;
    for (suffix, imul) in suffixes {
        if let Some((sv, rest)) = s.rsplit_once(suffix) {
            if !rest.is_empty() {
                anyhow::bail!("Trailing text after size: {rest}");
            }
            s = sv;
            mul = imul;
        }
    }
    let v = s.parse::<u64>()?;
    Ok(v * mul)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_size_mib() {
        let ident_cases = [0, 10, 9, 1024].into_iter().map(|k| (k.to_string(), k));
        let cases = [
            ("0M", 0),
            ("10M", 10),
            ("10MiB", 10),
            ("1G", 1024),
            ("9G", 9216),
            ("11T", 11 * 1024 * 1024),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v));
        for (s, v) in ident_cases.chain(cases) {
            assert_eq!(parse_size_mib(&s).unwrap(), v as u64, "Parsing {s}");
        }
    }

    #[test]
    fn test_parse_sfdisk() -> Result<()> {
        let fixture = indoc::indoc! { r#"
        {
            "partitiontable": {
               "label": "gpt",
               "id": "A67AA901-2C72-4818-B098-7F1CAC127279",
               "device": "/dev/loop0",
               "unit": "sectors",
               "firstlba": 34,
               "lastlba": 20971486,
               "sectorsize": 512,
               "partitions": [
                  {
                     "node": "/dev/loop0p1",
                     "start": 2048,
                     "size": 8192,
                     "type": "9E1A2D38-C612-4316-AA26-8B49521E5A8B",
                     "uuid": "58A4C5F0-BD12-424C-B563-195AC65A25DD",
                     "name": "PowerPC-PReP-boot"
                  },{
                     "node": "/dev/loop0p2",
                     "start": 10240,
                     "size": 20961247,
                     "type": "0FC63DAF-8483-4772-8E79-3D69D8477DE4",
                     "uuid": "F51ABB0D-DA16-4A21-83CB-37F4C805AAA0",
                     "name": "root"
                  }
               ]
            }
         }
        "# };
        let table: SfDiskOutput = serde_json::from_str(&fixture).unwrap();
        assert_eq!(
            table.partitiontable.find("/dev/loop0p2").unwrap().size,
            20961247
        );
        Ok(())
    }
}
