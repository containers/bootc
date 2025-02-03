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
use ostree_ext::ostree_prepareroot::Tristate;
use serde::{Deserialize, Serialize};

use crate::store::{self, Storage};

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VerityState {
    Enabled,
    Disabled,
    Inconsistent((u64, u64)),
}

#[derive(Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct FsckResult {
    pub(crate) notices: Vec<String>,
    pub(crate) errors: Vec<String>,
    pub(crate) verity: Option<VerityState>,
}

type Errors = Vec<String>;

/// Check the fsverity state of all regular files in this object directory.
#[context("Computing verity state")]
fn verity_state_of_objects(
    d: &Dir,
    prefix: &str,
    expected: Tristate,
) -> Result<(u64, u64, Errors)> {
    let mut enabled = 0;
    let mut disabled = 0;
    let mut errs = Errors::default();
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
            if expected == Tristate::Enabled {
                errs.push(format!(
                    "fsverity is not enabled for object: {prefix}{name}"
                ));
            }
        }
    }
    Ok((enabled, disabled, errs))
}

async fn verity_state_of_all_objects(
    repo: &ostree::Repo,
    expected: Tristate,
) -> Result<(u64, u64, Errors)> {
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
        let name = ent.file_name();
        let name = name
            .into_string()
            .map(Utf8PathBuf::from)
            .map_err(|_| anyhow::anyhow!("Invalid UTF-8"))?;

        let objdir = ent.open_dir()?;
        let expected = expected.clone();
        joinset.spawn_blocking(move || verity_state_of_objects(&objdir, name.as_str(), expected));
    }

    while let Some(output) = joinset.join_next().await {
        results.push(output??);
    }
    let r = results
        .into_iter()
        .fold((0, 0, Errors::default()), |mut acc, v| {
            acc.0 += v.0;
            acc.1 += v.1;
            acc.2.extend(v.2);
            acc
        });
    Ok(r)
}

pub(crate) async fn fsck(storage: &Storage) -> Result<FsckResult> {
    let mut r = FsckResult::default();

    let repo_config = storage.repo().config();
    let expected_verity = {
        let (k, v) = store::REPO_VERITY_CONFIG.split_once('.').unwrap();
        repo_config
            .optional_string(k, v)?
            .map(|v| Tristate::from_str(&v))
            .transpose()?
            .unwrap_or_default()
    };
    tracing::debug!("expected_verity={expected_verity:?}");

    let verity_found_state =
        verity_state_of_all_objects(&storage.repo(), expected_verity.clone()).await?;
    r.errors.extend(verity_found_state.2);
    r.verity = match (verity_found_state.0, verity_found_state.1) {
        (0, 0) => None,
        (_, 0) => Some(VerityState::Enabled),
        (0, _) => Some(VerityState::Disabled),
        (enabled, disabled) => Some(VerityState::Inconsistent((enabled, disabled))),
    };
    if let Some(VerityState::Inconsistent((enabled, disabled))) = r.verity {
        let inconsistent =
            format!("Inconsistent fsverity state (enabled: {enabled} disabled: {disabled})");
        match expected_verity {
            Tristate::Disabled | Tristate::Maybe => r.notices.push(inconsistent),
            Tristate::Enabled => r.errors.push(inconsistent),
        }
    }
    Ok(r)
}
