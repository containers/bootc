use anyhow::{Context, Result};
use ostree::gio;
use ostree_ext::ostree;
use ostree_ext::ostree::Deployment;
use ostree_ext::prelude::Cast;
use ostree_ext::prelude::FileEnumeratorExt;
use ostree_ext::prelude::FileExt;
use serde::Deserialize;

use crate::deploy::ImageState;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    kargs: Vec<String>,
    match_architectures: Option<Vec<String>>,
}

/// Compute the kernel arguments for the new deployment. This starts from the booted
/// karg, but applies the diff between the bootc karg files in /usr/lib/bootc/kargs.d
/// between the booted deployment and the new one.
pub(crate) fn get_kargs(
    repo: &ostree::Repo,
    booted_deployment: &Deployment,
    fetched: &ImageState,
) -> Result<Vec<String>> {
    let cancellable = gio::Cancellable::NONE;
    let mut kargs: Vec<String> = vec![];
    let sys_arch = std::env::consts::ARCH.to_string();

    // Get the running kargs of the booted system
    if let Some(bootconfig) = ostree::Deployment::bootconfig(booted_deployment) {
        if let Some(options) = ostree::BootconfigParser::get(&bootconfig, "options") {
            let options: Vec<&str> = options.split_whitespace().collect();
            let mut options: Vec<String> = options.into_iter().map(|s| s.to_string()).collect();
            kargs.append(&mut options);
        }
    };

    // Get the kargs in kargs.d of the booted system
    let mut existing_kargs: Vec<String> = vec![];
    let fragments = liboverdrop::scan(&["/usr/lib"], "bootc/kargs.d", &["toml"], true);
    for (name, path) in fragments {
        let s = std::fs::read_to_string(&path)?;
        let mut parsed_kargs =
            parse_kargs_toml(&s, &sys_arch).with_context(|| format!("Parsing {name:?}"))?;
        existing_kargs.append(&mut parsed_kargs);
    }

    // Get the kargs in kargs.d of the pending image
    let mut remote_kargs: Vec<String> = vec![];
    let (fetched_tree, _) = repo.read_commit(fetched.ostree_commit.as_str(), cancellable)?;
    let fetched_tree = fetched_tree.resolve_relative_path("/usr/lib/bootc/kargs.d");
    let fetched_tree = fetched_tree
        .downcast::<ostree::RepoFile>()
        .expect("downcast");
    if !fetched_tree.query_exists(cancellable) {
        // if the kargs.d directory does not exist in the fetched image, return the existing kargs
        kargs.append(&mut existing_kargs);
        return Ok(kargs);
    }
    let queryattrs = "standard::name,standard::type";
    let queryflags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;
    let fetched_iter = fetched_tree.enumerate_children(queryattrs, queryflags, cancellable)?;
    while let Some(fetched_info) = fetched_iter.next_file(cancellable)? {
        // only read and parse the file if it is a toml file
        let name = fetched_info.name();
        if let Some(name) = name.to_str() {
            if name.ends_with(".toml") {
                let fetched_child = fetched_iter.child(&fetched_info);
                let fetched_child = fetched_child
                    .downcast::<ostree::RepoFile>()
                    .expect("downcast");
                fetched_child.ensure_resolved()?;
                let fetched_contents_checksum = fetched_child.checksum();
                let f =
                    ostree::Repo::load_file(repo, fetched_contents_checksum.as_str(), cancellable)?;
                let file_content = f.0;
                let mut reader =
                    ostree_ext::prelude::InputStreamExtManual::into_read(file_content.unwrap());
                let s = std::io::read_to_string(&mut reader)?;
                let mut parsed_kargs =
                    parse_kargs_toml(&s, &sys_arch).with_context(|| format!("Parsing {name}"))?;
                remote_kargs.append(&mut parsed_kargs);
            }
        }
    }

    // get the diff between the existing and remote kargs
    let mut added_kargs: Vec<String> = remote_kargs
        .clone()
        .into_iter()
        .filter(|item| !existing_kargs.contains(item))
        .collect();
    let removed_kargs: Vec<String> = existing_kargs
        .clone()
        .into_iter()
        .filter(|item| !remote_kargs.contains(item))
        .collect();

    tracing::debug!(
        "kargs: added={:?} removed={:?}",
        &added_kargs,
        removed_kargs
    );

    // apply the diff to the system kargs
    kargs.retain(|x| !removed_kargs.contains(x));
    kargs.append(&mut added_kargs);

    Ok(kargs)
}

/// This parses a bootc kargs.d toml file, returning the resulting
/// vector of kernel arguments. Architecture matching is performed using
/// `sys_arch`.
fn parse_kargs_toml(contents: &str, sys_arch: &str) -> Result<Vec<String>> {
    let mut de: Config = toml::from_str(contents)?;
    let mut parsed_kargs: Vec<String> = vec![];
    // if arch specified, apply kargs only if the arch matches
    // if arch not specified, apply kargs unconditionally
    match de.match_architectures {
        None => parsed_kargs = de.kargs,
        Some(match_architectures) => {
            if match_architectures.iter().any(|s| s == sys_arch) {
                parsed_kargs.append(&mut de.kargs);
            }
        }
    }
    Ok(parsed_kargs)
}

#[test]
/// Verify that kargs are only applied to supported architectures
fn test_arch() {
    // no arch specified, kargs ensure that kargs are applied unconditionally
    let sys_arch = "x86_64".to_string();
    let file_content = r##"kargs = ["console=tty0", "nosmt"]"##.to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, &sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);
    let sys_arch = "aarch64".to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, &sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);

    // one arch matches and one doesn't, ensure that kargs are only applied for the matching arch
    let sys_arch = "aarch64".to_string();
    let file_content = r##"kargs = ["console=tty0", "nosmt"]
match-architectures = ["x86_64"]
"##
    .to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, &sys_arch).unwrap();
    assert_eq!(parsed_kargs, [] as [String; 0]);
    let file_content = r##"kargs = ["console=tty0", "nosmt"]
match-architectures = ["aarch64"]
"##
    .to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, &sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);

    // multiple arch specified, ensure that kargs are applied to both archs
    let sys_arch = "x86_64".to_string();
    let file_content = r##"kargs = ["console=tty0", "nosmt"]
match-architectures = ["x86_64", "aarch64"]
"##
    .to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, &sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);
    std::env::set_var("ARCH", "aarch64");
    let parsed_kargs = parse_kargs_toml(&file_content, &sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);
}
