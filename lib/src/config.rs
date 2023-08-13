use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use anyhow::{anyhow, Context, Result};
use camino::Utf8Path;
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use fn_error_context::context;
use k8s_openapi::{api::core::v1::ConfigMap, http::HeaderValue};
use ostree_ext::container as ostree_container;
use ostree_ext::prelude::{Cast, FileExt, InputStreamExtManual, ToVariant};
use ostree_ext::{gio, glib, ostree};
use ostree_ext::{ostree::Deployment, sysroot::SysrootLock};
use reqwest::StatusCode;
use rustix::fd::AsRawFd;

use crate::deploy::require_base_commit;

/// The prefix used to store configmaps
const REF_PREFIX: &str = "bootc/config";

/// The key used to configure the file prefix; the default is `/etc`.
const CONFIGMAP_PREFIX_ANNOTATION_KEY: &str = "bootc.prefix";
/// The default prefix for configmaps and secrets.
const DEFAULT_MOUNT_PREFIX: &str = "etc";

/// The key used to store the configmap metadata
const CONFIGMAP_METADATA_KEY: &str = "bootc.configmap.metadata";
/// The key used to store the etag from the HTTP request
const CONFIGMAP_ETAG_KEY: &str = "bootc.configmap.etag";

/// Default to world-readable for configmaps
const DEFAULT_MODE: u32 = 0o644;

const ORIGIN_BOOTC_CONFIG_PREFIX: &str = "bootc.config.";

/// The serialized metadata about configmaps attached to a deployment
pub(crate) struct ConfigSpec {
    pub(crate) name: String,
    pub(crate) url: String,
}

impl ConfigSpec {
    const KEY_URL: &str = "url";

    /// Return the keyfile group name
    fn group(name: &str) -> String {
        format!("{ORIGIN_BOOTC_CONFIG_PREFIX}{name}")
    }

    /// Parse a config specification from a keyfile
    #[context("Parsing config spec")]
    fn from_keyfile(kf: &glib::KeyFile, name: &str) -> Result<Self> {
        let group = Self::group(name);
        let url = kf.string(&group, Self::KEY_URL)?.to_string();
        Ok(Self {
            url,
            name: name.to_string(),
        })
    }

    /// Serialize this config spec into the target keyfile
    fn store(&self, kf: &glib::KeyFile) {
        let group = &Self::group(&self.name);
        // Ignore errors if the group didn't exist
        let _ = kf.remove_group(group);
        kf.set_string(group, Self::KEY_URL, &self.url);
    }

    /// Remove this config from the target; returns `true` if the value was present
    fn remove(&self, kf: &glib::KeyFile) -> bool {
        let group = &Self::group(&self.name);
        kf.remove_group(group).is_ok()
    }

    pub(crate) fn ostree_ref(&self) -> Result<String> {
        name_to_ostree_ref(&self.name)
    }
}

/// Options for internal testing
#[derive(Debug, clap::Subcommand)]
pub(crate) enum ConfigOpts {
    /// Add a remote configmap
    AddFromURL {
        /// Remote URL for configmap
        url: String,

        #[clap(long)]
        /// Provide an explicit name for the map
        name: Option<String>,
    },
    /// Show a configmap (in YAML format)
    Show {
        /// Name of the configmap to show
        name: String,
    },
    /// Add a remote configmap
    Remove {
        /// Name of the configmap to remove
        name: String,
    },
    /// Check for updates for an individual configmap
    Update {
        /// Name of the configmap to update
        names: Vec<String>,
    },
    /// List attached configmaps
    List,
}

/// Implementation of the `boot config` CLI.
pub(crate) async fn run(opts: ConfigOpts) -> Result<()> {
    crate::cli::prepare_for_write().await?;
    let sysroot = &crate::cli::get_locked_sysroot().await?;
    match opts {
        ConfigOpts::AddFromURL { url, name } => add_from_url(sysroot, &url, name.as_deref()).await,
        ConfigOpts::Remove { name } => remove(sysroot, name.as_str()).await,
        ConfigOpts::Update { names } => update(sysroot, names.into_iter()).await,
        ConfigOpts::Show { name } => show(sysroot, &name).await,
        ConfigOpts::List => list(sysroot).await,
    }
}

#[context("Converting configmap name to ostree ref")]
fn name_to_ostree_ref(name: &str) -> Result<String> {
    ostree_ext::refescape::prefix_escape_for_ref(REF_PREFIX, name)
}

/// Retrieve the "mount prefix" for the configmap
fn get_prefix(map: &ConfigMap) -> &str {
    map.metadata
        .annotations
        .as_ref()
        .and_then(|m| m.get(CONFIGMAP_PREFIX_ANNOTATION_KEY).map(|s| s.as_str()))
        .unwrap_or(DEFAULT_MOUNT_PREFIX)
}

async fn list(sysroot: &SysrootLock) -> Result<()> {
    let merge_deployment = &crate::cli::target_deployment(sysroot)?;
    let configs = configs_for_deployment(sysroot, merge_deployment)?;
    if configs.len() == 0 {
        println!("No dynamic ConfigMap objects attached");
    } else {
        for config in configs {
            let name = config.name;
            let url = config.url;
            println!("{name} {url}");
        }
    }
    Ok(())
}

fn load_config(sysroot: &SysrootLock, name: &str) -> Result<ConfigMap> {
    let cancellable = gio::Cancellable::NONE;
    let configref = name_to_ostree_ref(name)?;
    let (r, rev) = sysroot.repo().read_commit(&configref, cancellable)?;
    tracing::debug!("Inspecting {rev}");
    let commitv = sysroot.repo().load_commit(&rev)?.0;
    let commitmeta = commitv.child_value(0);
    let commitmeta = &glib::VariantDict::new(Some(&commitmeta));
    let cfgdata = commitmeta
        .lookup_value(CONFIGMAP_METADATA_KEY, Some(glib::VariantTy::STRING))
        .ok_or_else(|| anyhow!("Missing metadata key {CONFIGMAP_METADATA_KEY}"))?;
    let cfgdata = cfgdata.str().unwrap();
    let mut cfg: ConfigMap = serde_json::from_str(cfgdata)?;
    let prefix = Utf8Path::new(get_prefix(&cfg).trim_start_matches('/'));
    let d = r.child(prefix);
    if let Some(v) = cfg.binary_data.as_mut() {
        for (k, v) in v.iter_mut() {
            let k = k.trim_start_matches('/');
            d.child(k)
                .read(cancellable)?
                .into_read()
                .read_to_end(&mut v.0)?;
        }
    }
    if let Some(v) = cfg.data.as_mut() {
        for (k, v) in v.iter_mut() {
            let k = k.trim_start_matches('/');
            d.child(k)
                .read(cancellable)?
                .into_read()
                .read_to_string(v)?;
        }
    }
    Ok(cfg)
}

async fn show(sysroot: &SysrootLock, name: &str) -> Result<()> {
    let config = load_config(sysroot, name)?;
    let mut stdout = std::io::stdout().lock();
    serde_yaml::to_writer(&mut stdout, &config)?;
    Ok(())
}

async fn remove(sysroot: &SysrootLock, name: &str) -> Result<()> {
    let cancellable = gio::Cancellable::NONE;
    let repo = &sysroot.repo();
    let merge_deployment = &crate::cli::target_deployment(sysroot)?;
    let stateroot = merge_deployment.osname();
    let origin = merge_deployment
        .origin()
        .ok_or_else(|| anyhow::anyhow!("Deployment is missing an origin"))?;
    let configs = configs_for_deployment(sysroot, merge_deployment)?;
    let cfgspec = configs
        .iter()
        .find(|v| v.name == name)
        .ok_or_else(|| anyhow::anyhow!("No config with name {name}"))?;
    let removed = cfgspec.remove(&origin);
    assert!(removed);

    let cfgref = cfgspec.ostree_ref()?;
    tracing::debug!("Removing ref {cfgref}");
    repo.set_ref_immediate(None, &cfgref, None, cancellable)?;

    let merge_commit = merge_deployment.csum();
    let commit = require_base_commit(repo, &merge_commit)?;
    let state = ostree_container::store::query_image_commit(repo, &commit)?;
    crate::deploy::deploy(sysroot, Some(merge_deployment), &stateroot, state, &origin).await?;
    crate::deploy::cleanup(sysroot).await?;
    println!("Queued changes for next boot");

    Ok(())
}

#[derive(Debug)]
struct HttpCachableReply<T> {
    content: T,
    etag: Option<String>,
}

#[context("Writing configmap")]
fn write_configmap(
    sysroot: &SysrootLock,
    sepolicy: Option<&ostree::SePolicy>,
    spec: &ConfigSpec,
    map: &ConfigMap,
    etag: Option<&str>,
    cancellable: Option<&gio::Cancellable>,
) -> Result<()> {
    use crate::ostree_generation::{create_and_commit_dirmeta, write_file};
    let name = spec.name.as_str();
    tracing::debug!("Writing configmap {name}");
    let oref = name_to_ostree_ref(&spec.name)?;
    let repo = &sysroot.repo();
    let tx = repo.auto_transaction(cancellable)?;
    let tree = &ostree::MutableTree::new();
    let dirmeta =
        create_and_commit_dirmeta(&repo, "/etc/some-unshipped-config-file".into(), sepolicy)?;
    // Create an iterator over the string data
    let string_data = map.data.iter().flatten().map(|(k, v)| (k, v.as_bytes()));
    // Create an iterator over the binary data
    let binary_data = map
        .binary_data
        .iter()
        .flatten()
        .map(|(k, v)| (k, v.0.as_slice()));
    let prefix = get_prefix(map);
    tracing::trace!("prefix={prefix}");
    // For each string and binary value, write a file
    let mut has_content = false;
    for (k, v) in string_data.chain(binary_data) {
        let path = Utf8Path::new(prefix).join(k);
        tracing::trace!("Writing {path}");
        write_file(repo, tree, &path, &dirmeta, v, DEFAULT_MODE, sepolicy)?;
        has_content = true;
    }
    if !has_content {
        anyhow::bail!("ConfigMap has no data");
    }
    // Empty out the values, since we wrote them into the ostree commit on the filesystem
    let binary_data = map.binary_data.as_ref().map(|v| {
        v.keys()
            .map(|k| (k.clone(), k8s_openapi::ByteString(Vec::new())))
            .collect::<BTreeMap<_, _>>()
    });
    let data = map.data.as_ref().map(|v| {
        v.keys()
            .map(|k| (k.clone(), "".to_string()))
            .collect::<BTreeMap<_, _>>()
    });
    let rest = ConfigMap {
        binary_data,
        data,
        immutable: map.immutable.clone(),
        metadata: map.metadata.clone(),
    };
    let serialized_map_metadata =
        serde_json::to_string(&rest).context("Serializing configmap metadata")?;
    let mut metadata = HashMap::new();
    metadata.insert(CONFIGMAP_METADATA_KEY, serialized_map_metadata.to_variant());
    if let Some(etag) = etag {
        metadata.insert(CONFIGMAP_ETAG_KEY, etag.to_variant());
    }
    let timestamp = map
        .metadata
        .creation_timestamp
        .as_ref()
        .map(|t| t.0.timestamp() as u64)
        .unwrap_or_default();
    tracing::trace!("Writing commit with ts {timestamp}");

    let root = repo.write_mtree(&tree, cancellable)?;
    let root = root.downcast_ref::<ostree::RepoFile>().unwrap();
    let commit = repo.write_commit_with_time(
        None,
        None,
        None,
        Some(&metadata.to_variant()),
        root,
        timestamp,
        cancellable,
    )?;
    repo.transaction_set_ref(None, &oref, Some(commit.as_str()));
    tx.commit(cancellable)?;

    Ok(())
}

#[context("Fetching configmap from {url}")]
/// Download a configmap, honoring an optional ETag.  If the server says the resource
/// is unmodified, this returns `Ok(None)`.
async fn fetch_configmap(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
) -> Result<Option<HttpCachableReply<ConfigMap>>> {
    tracing::debug!("Fetching {url}");
    let mut req = client.get(url);
    if let Some(etag) = etag {
        tracing::trace!("Providing etag {etag}");
        let val = HeaderValue::from_str(etag).context("Parsing etag")?;
        req = req.header(reqwest::header::IF_NONE_MATCH, val);
    }
    let reply = req.send().await?;
    if reply.status() == StatusCode::NOT_MODIFIED {
        tracing::debug!("Server returned NOT_MODIFIED");
        return Ok(None);
    }
    let etag = reply
        .headers()
        .get(reqwest::header::ETAG)
        .map(|v| v.to_str())
        .transpose()
        .context("Parsing etag")?
        .map(ToOwned::to_owned);
    // TODO: streaming deserialize
    let buf = reply.bytes().await?;
    tracing::trace!("Parsing server reply of {} bytes", buf.len());
    serde_yaml::from_slice(&buf)
        .context("Deserializing configmap")
        .map(|v| Some(HttpCachableReply { content: v, etag }))
}

/// Download a configmap.
async fn fetch_required_configmap(
    client: &reqwest::Client,
    url: &str,
) -> Result<HttpCachableReply<ConfigMap>> {
    fetch_configmap(client, url, None)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Server unexpectedly returned unmodified status"))
}

/// Return the attached configmaps for a deployment.
#[context("Querying config names")]
pub(crate) fn configs_for_deployment(
    _sysroot: &SysrootLock,
    deployment: &Deployment,
) -> Result<Vec<ConfigSpec>> {
    let origin = deployment
        .origin()
        .ok_or_else(|| anyhow::anyhow!("Deployment is missing an origin"))?;
    origin
        .groups()
        .0
        .into_iter()
        .try_fold(Vec::new(), |mut acc, name| {
            if let Some(name) = name.strip_prefix(ORIGIN_BOOTC_CONFIG_PREFIX) {
                let spec = ConfigSpec::from_keyfile(&origin, name)?;
                acc.push(spec);
            }
            anyhow::Ok(acc)
        })
}

async fn add_from_url(sysroot: &SysrootLock, url: &str, name: Option<&str>) -> Result<()> {
    let cancellable = gio::Cancellable::NONE;
    let repo = &sysroot.repo();
    let merge_deployment = &crate::cli::target_deployment(sysroot)?;
    let stateroot = merge_deployment.osname();
    let client = crate::utils::new_http_client().build()?;
    let reply = fetch_required_configmap(&client, url).await?;
    let configmap = reply.content;
    let origin = merge_deployment
        .origin()
        .ok_or_else(|| anyhow::anyhow!("Deployment is missing an origin"))?;
    let dirpath = sysroot.deployment_dirpath(merge_deployment);
    // SAFETY: None of this should be NULL
    let dirpath = sysroot.path().path().unwrap().join(dirpath);
    let deployment_fd = Dir::open_ambient_dir(&dirpath, cap_std::ambient_authority())
        .with_context(|| format!("Opening deployment directory {dirpath:?}"))?;
    let sepolicy = ostree::SePolicy::new_at(deployment_fd.as_raw_fd(), cancellable)?;
    let name = name
        .or_else(|| configmap.metadata.name.as_deref())
        .ok_or_else(|| anyhow!("Missing metadata.name and no name provided"))?;
    let configs = configs_for_deployment(sysroot, merge_deployment)?;
    if configs.iter().any(|v| v.name == name) {
        anyhow::bail!("Already have a config with name {name}");
    }
    let spec = ConfigSpec {
        name: name.to_owned(),
        url: url.to_owned(),
    };
    let oref = name_to_ostree_ref(name)?;
    tracing::trace!("configmap {name} => {oref}");
    // TODO use ostree_ext::tokio_util::spawn_blocking_cancellable_flatten(move |cancellable| {
    // once https://github.com/ostreedev/ostree/pull/2824 lands
    write_configmap(
        sysroot,
        Some(&sepolicy),
        &spec,
        &configmap,
        reply.etag.as_deref(),
        cancellable,
    )?;
    println!("Stored configmap: {name}");

    spec.store(&origin);

    let merge_commit = merge_deployment.csum();
    let commit = require_base_commit(repo, &merge_commit)?;
    let state = ostree_container::store::query_image_commit(repo, &commit)?;
    crate::deploy::deploy(sysroot, Some(merge_deployment), &stateroot, state, &origin).await?;
    crate::deploy::cleanup(sysroot).await?;
    println!("Queued changes for next boot");

    Ok(())
}

async fn update_one_config(
    sysroot: &SysrootLock,
    merge_deployment: &ostree::Deployment,
    configs: &[&ConfigSpec],
    name: &str,
    httpclient: &reqwest::Client,
) -> Result<bool> {
    let cancellable = gio::Cancellable::NONE;
    let repo = &sysroot.repo();
    let cfgspec = configs
        .into_iter()
        .find(|v| v.name == name)
        .ok_or_else(|| anyhow::anyhow!("No config with name {name}"))?;
    let cfgref = cfgspec.ostree_ref()?;
    let cfg_commit = repo.require_rev(&cfgref)?;
    let cfg_commitv = repo.load_commit(&cfg_commit)?.0;
    let cfg_commitmeta = glib::VariantDict::new(Some(&cfg_commitv.child_value(0)));
    let etag = cfg_commitmeta
        .lookup::<String>(CONFIGMAP_ETAG_KEY)?
        .ok_or_else(|| anyhow!("Missing {CONFIGMAP_ETAG_KEY}"))?;
    let reply = match fetch_configmap(httpclient, &cfgspec.url, Some(etag.as_str())).await? {
        Some(v) => v,
        None => {
            return Ok(false);
        }
    };
    let dirpath = sysroot.deployment_dirpath(merge_deployment);
    // SAFETY: None of this should be NULL
    let dirpath = sysroot.path().path().unwrap().join(dirpath);
    let deployment_fd = Dir::open_ambient_dir(&dirpath, cap_std::ambient_authority())
        .with_context(|| format!("Opening deployment directory {dirpath:?}"))?;
    let sepolicy = ostree::SePolicy::new_at(deployment_fd.as_raw_fd(), cancellable)?;
    write_configmap(
        sysroot,
        Some(&sepolicy),
        cfgspec,
        &reply.content,
        reply.etag.as_deref(),
        cancellable,
    )?;
    Ok(true)
}

async fn update<S: AsRef<str>>(
    sysroot: &SysrootLock,
    names: impl Iterator<Item = S>,
) -> Result<()> {
    let merge_deployment = &crate::cli::target_deployment(sysroot)?;
    let origin = merge_deployment
        .origin()
        .ok_or_else(|| anyhow::anyhow!("Deployment is missing an origin"))?;
    let configs = configs_for_deployment(sysroot, merge_deployment)?;
    let configs = configs.iter().collect::<Vec<_>>();
    let httpclient = &crate::utils::new_http_client().build()?;
    let mut changed = false;
    for name in names {
        let name = name.as_ref();
        if update_one_config(
            sysroot,
            merge_deployment,
            configs.as_slice(),
            name,
            httpclient,
        )
        .await?
        {
            println!("Updated configmap {name}");
            changed = true;
        } else {
            println!("No changes in configmap {name}");
        }
    }

    if !changed {
        return Ok(());
    }

    let repo = &sysroot.repo();
    let stateroot = &merge_deployment.osname();
    let merge_commit = merge_deployment.csum();
    let commit = require_base_commit(repo, &merge_commit)?;
    let state = ostree_container::store::query_image_commit(repo, &commit)?;
    crate::deploy::deploy(sysroot, Some(merge_deployment), &stateroot, state, &origin).await?;
    crate::deploy::cleanup(sysroot).await?;
    println!("Queued changes for next boot");

    Ok(())
}
