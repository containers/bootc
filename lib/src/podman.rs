use std::collections::HashSet;
use std::io::{BufReader, Read};

use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;
use fn_error_context::context;
use serde::Deserialize;

use crate::install::run_in_host_mountns;
use crate::task::Task;

/// Where we look inside our container to find our own image
/// for use with `bootc install`.
pub(crate) const CONTAINER_STORAGE: &str = "/var/lib/containers";
/// Currently a magic comment which instructs bootc it should pull these
/// images.
pub(crate) const BOOTC_BOUND_FLAG: &str = "# bootc: bound";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Inspect {
    pub(crate) digest: String,
}

/// Given an image ID, return its manifest digest
pub(crate) fn imageid_to_digest(imgid: &str) -> Result<String> {
    let out = Task::new_cmd("podman inspect", run_in_host_mountns("podman"))
        .args(["inspect", imgid])
        .quiet()
        .read()?;
    let o: Vec<Inspect> = serde_json::from_str(&out)?;
    let i = o
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No images returned for inspect"))?;
    Ok(i.digest)
}

/// List all container `.image` files described in the target root
#[context("Listing .image files")]
pub(crate) fn list_container_images(root: &Dir) -> Result<(usize, HashSet<String>)> {
    const ETC_ROOT: &str = "etc/containers/systemd";
    const USR_ROOT: &str = "usr/share/containers/systemd";

    let mut found_image_files = 0;
    let mut r = HashSet::new();
    for d in [ETC_ROOT, USR_ROOT] {
        let imagedir = if let Some(d) = root.open_dir_optional(d)? {
            d
        } else {
            tracing::debug!("No {d} found");
            continue;
        };
        for entry in imagedir.entries()? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = if let Some(n) = name.to_str() {
                n
            } else {
                anyhow::bail!("Invalid non-UTF8 filename: {name:?} in {d}");
            };
            if !matches!(Utf8Path::new(name).extension(), Some("image")) {
                continue;
            }
            found_image_files += 1;
            let mut buf = String::new();
            entry.open().map(BufReader::new)?.read_to_string(&mut buf)?;
            let mut is_bound = false;
            for line in buf.lines() {
                if line.starts_with(BOOTC_BOUND_FLAG) {
                    is_bound = true;
                    break;
                }
            }
            if !is_bound {
                tracing::trace!("{name}: Did not find {BOOTC_BOUND_FLAG}");
                continue;
            }
            let config = ini::Ini::load_from_str(&buf).with_context(|| format!("{name}:"))?;
            let image = if let Some(img) = config.get_from(Some("Container"), "Image") {
                img
            } else {
                tracing::debug!("{name}: Missing Container/Image key");
                continue;
            };
            tracing::trace!("{name}: Bound {image}");
            r.insert(image.to_string());
        }
    }
    Ok((found_image_files, r))
}
