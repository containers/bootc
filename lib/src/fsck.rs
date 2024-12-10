//! # Write deployments merging image with configmap
//!
//! Create a merged filesystem tree with the image and mounted configmaps.

use std::os::fd::AsFd;
use std::str::FromStr as _;

use anyhow::Ok;
use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use fn_error_context::context;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;
use serde::{Deserialize, Serialize};

use crate::install::config::Tristate;
use crate::store::{self, Storage};

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VerityState {
    Enabled,
    Disabled,
    Inconsistent,
}

#[derive(Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct FsckResult {
    pub(crate) notices: Vec<String>,
    pub(crate) errors: Vec<String>,
    pub(crate) verity: Option<VerityState>,
}

/// Check the fsverity state of all regular files in this object directory.
#[context("Computing verity state")]
fn verity_state_of_objects(d: &Dir) -> Result<(u64, u64)> {
    let mut enabled = 0;
    let mut disabled = 0;
    for ent in d.entries()? {
        let ent = ent?;
        if !ent.file_type()?.is_file() {
            continue;
        }
        let name = ent.file_name();
        let name = name
            .into_string()
            .map(Utf8PathBuf::from)
            .map_err(|_| anyhow::anyhow!("Invalid UTF-8"))?;
        let Some("file") = name.extension() else {
            continue;
        };
        let f = d
            .open(&name)
            .with_context(|| format!("Failed to open {name}"))?;
        let r: Option<composefs::fsverity::Sha256HashValue> =
            composefs::fsverity::ioctl::fs_ioc_measure_verity(f.as_fd())?;
        drop(f);
        if r.is_some() {
            enabled += 1;
        } else {
            disabled += 1;
        }
    }
    Ok((enabled, disabled))
}

async fn verity_state_of_all_objects(repo: &ostree::Repo) -> Result<(u64, u64)> {
    const MAX_CONCURRENT: usize = 3;

    let repodir = Dir::reopen_dir(&repo.dfd_borrow())?;

    let mut joinset = tokio::task::JoinSet::new();
    let mut results = Vec::new();

    for ent in repodir.read_dir("objects")? {
        while joinset.len() >= MAX_CONCURRENT {
            results.push(joinset.join_next().await.unwrap()??);
        }
        let ent = ent?;
        if !ent.file_type()?.is_dir() {
            continue;
        }
        let objdir = ent.open_dir()?;
        joinset.spawn_blocking(move || verity_state_of_objects(&objdir));
    }

    while let Some(output) = joinset.join_next().await {
        results.push(output??);
    }
    let r = results.into_iter().fold((0, 0), |mut acc, v| {
        acc.0 += v.0;
        acc.1 += v.1;
        acc
    });
    Ok(r)
}

pub(crate) async fn fsck(storage: &Storage) -> Result<FsckResult> {
    let mut r = FsckResult::default();

    let repo_config = storage.repo().config();
    let verity_state = {
        let (k, v) = store::REPO_VERITY_CONFIG.split_once('.').unwrap();
        repo_config
            .optional_string(k, v)?
            .map(|v| Tristate::from_str(&v))
            .transpose()?
            .unwrap_or_default()
    };

    r.verity = match verity_state_of_all_objects(&storage.repo()).await? {
        (0, 0) => None,
        (_, 0) => Some(VerityState::Enabled),
        (0, _) => Some(VerityState::Disabled),
        _ => Some(VerityState::Inconsistent),
    };
    if matches!(&r.verity, &Some(VerityState::Inconsistent)) {
        let inconsistent = "Inconsistent fsverity state".to_string();
        match verity_state {
            Tristate::Disabled | Tristate::Maybe => r.notices.push(inconsistent),
            Tristate::Enabled => r.errors.push(inconsistent),
        }
    }
    serde_json::to_writer(std::io::stdout().lock(), &r)?;
    Ok(r)
}
