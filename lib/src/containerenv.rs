//! Helpers for parsing the `/run/.containerenv` file generated by podman.

use std::io::{BufRead, BufReader};

use anyhow::Result;
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::prelude::CapStdExtDirExt;
use fn_error_context::context;

const PATH: &str = "run/.containerenv";

#[derive(Debug, Default)]
pub(crate) struct ContainerExecutionInfo {
    pub(crate) engine: String,
    pub(crate) name: String,
    pub(crate) id: String,
    pub(crate) image: String,
    pub(crate) imageid: String,
    pub(crate) rootless: Option<String>,
}

/// Load and parse the `/run/.containerenv` file.
#[context("Querying container")]
pub(crate) fn get_container_execution_info(rootfs: &Dir) -> Result<ContainerExecutionInfo> {
    let f = match rootfs.open_optional(PATH)? {
        Some(f) => BufReader::new(f),
        None => {
            anyhow::bail!("This command must be executed inside a podman container (missing {PATH}")
        }
    };
    let mut r = ContainerExecutionInfo::default();
    for line in f.lines() {
        let line = line?;
        let line = line.trim();
        let (k, v) = if let Some(v) = line.split_once('=') {
            v
        } else {
            continue;
        };
        // Assuming there's no quotes here
        let v = v.trim_start_matches('"').trim_end_matches('"');
        match k {
            "engine" => r.engine = v.to_string(),
            "name" => r.name = v.to_string(),
            "id" => r.id = v.to_string(),
            "image" => r.image = v.to_string(),
            "imageid" => r.imageid = v.to_string(),
            _ => {}
        }
    }
    Ok(r)
}
