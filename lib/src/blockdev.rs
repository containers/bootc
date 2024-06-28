use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use nix::errno::Errno;
use regex::Regex;
use serde::Deserialize;

use crate::install::run_in_host_mountns;
use crate::task::Task;

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
    pub(crate) label: Option<String>,
    pub(crate) fstype: Option<String>,
    pub(crate) children: Option<Vec<Device>>,
    pub(crate) size: Option<String>,
}

impl Device {
    #[allow(dead_code)]
    // RHEL8's lsblk doesn't have PATH, so we do it
    pub(crate) fn path(&self) -> String {
        format!("/dev/{}", &self.name)
    }

    pub(crate) fn has_children(&self) -> bool {
        self.children.as_ref().map_or(false, |v| !v.is_empty())
    }
}

#[context("Failed to wipe {dev}")]
pub(crate) fn wipefs(dev: &Utf8Path) -> Result<()> {
    Task::new_and_run(
        format!("Wiping device {dev}"),
        "wipefs",
        ["-a", dev.as_str()],
    )
}

fn list_impl(dev: Option<&Utf8Path>) -> Result<Vec<Device>> {
    let o = Command::new("lsblk")
        .args(["-J", "-o", "NAME,SERIAL,MODEL,LABEL,FSTYPE,SIZE"])
        .args(dev)
        .output()?;
    if !o.status.success() {
        return Err(anyhow::anyhow!("Failed to list block devices"));
    }
    let devs: DevicesOutput = serde_json::from_reader(&*o.stdout)?;
    Ok(devs.blockdevices)
}

#[context("Listing device {dev}")]
pub(crate) fn list_dev(dev: &Utf8Path) -> Result<Device> {
    let devices = list_impl(Some(dev))?;
    devices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no device output from lsblk for {dev}"))
}

#[allow(dead_code)]
pub(crate) fn list() -> Result<Vec<Device>> {
    list_impl(None)
}

pub(crate) struct LoopbackDevice {
    pub(crate) dev: Option<Utf8PathBuf>,
}

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

        let dev = Task::new("losetup", "losetup")
            .args([
                "--show",
                format!("--direct-io={direct_io}").as_str(),
                "-P",
                "--find",
            ])
            .arg(path)
            .quiet()
            .read()?;
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
        Task::new("losetup", "losetup")
            .args(["-d", dev.as_str()])
            .quiet()
            .run()
    }

    /// Consume this device, unmounting it.
    pub(crate) fn close(mut self) -> Result<()> {
        self.impl_close()
    }
}

impl Drop for LoopbackDevice {
    fn drop(&mut self) {
        // Best effort to unmount if we're dropped without invoking `close`
        let _ = self.impl_close();
    }
}

pub(crate) fn udev_settle() -> Result<()> {
    // There's a potential window after rereading the partition table where
    // udevd hasn't yet received updates from the kernel, settle will return
    // immediately, and lsblk won't pick up partition labels.  Try to sleep
    // our way out of this.
    std::thread::sleep(std::time::Duration::from_millis(200));

    let st = run_in_host_mountns("udevadm").arg("settle").status()?;
    if !st.success() {
        anyhow::bail!("Failed to run udevadm settle: {st:?}");
    }
    Ok(())
}

#[allow(unsafe_code)]
pub(crate) fn reread_partition_table(file: &mut File, retry: bool) -> Result<()> {
    let fd = file.as_raw_fd();
    // Reread sometimes fails inexplicably.  Retry several times before
    // giving up.
    let max_tries = if retry { 20 } else { 1 };
    for retries in (0..max_tries).rev() {
        let result = unsafe { ioctl::blkrrpart(fd) };
        match result {
            Ok(_) => break,
            Err(err) if retries == 0 && err == Errno::EINVAL => {
                return Err(err)
                    .context("couldn't reread partition table: device may not support partitions")
            }
            Err(err) if retries == 0 && err == Errno::EBUSY => {
                return Err(err).context("couldn't reread partition table: device is in use")
            }
            Err(err) if retries == 0 => return Err(err).context("couldn't reread partition table"),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    Ok(())
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
    let output = Task::new_quiet("lsblk")
        // Older lsblk, e.g. in CentOS 7.6, doesn't support PATH, but --paths option
        .arg("--pairs")
        .arg("--paths")
        .arg("--inverse")
        .arg("--output")
        .arg("NAME,TYPE")
        .arg(device)
        .read()?;
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

// create unsafe ioctl wrappers
#[allow(clippy::missing_safety_doc)]
mod ioctl {
    use libc::c_int;
    use nix::{ioctl_none, ioctl_read, ioctl_read_bad, libc, request_code_none};
    ioctl_none!(blkrrpart, 0x12, 95);
    ioctl_read_bad!(blksszget, request_code_none!(0x12, 104), c_int);
    ioctl_read!(blkgetsize64, 0x12, 114, libc::size_t);
}

/// Parse a string into mibibytes
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
