use crate::task::Task;
use crate::utils::run_in_host_mountns;
use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
use fn_error_context::context;
use nix::errno::Errno;
use serde::Deserialize;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::process::Command;

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

pub(crate) fn wipefs(dev: &Utf8Path) -> Result<()> {
    Task::new_and_run(
        &format!("Wiping device {dev}"),
        "wipefs",
        ["-a", dev.as_str()],
    )
}

fn list_impl(dev: Option<&Utf8Path>) -> Result<Vec<Device>> {
    let o = Command::new("lsblk")
        .args(["-J", "-o", "NAME,SERIAL,MODEL,LABEL,FSTYPE"])
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
