use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std_ext::cap_std;
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;
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

/// Load and parse all bootc kargs.d files in the specified root, returning
/// a combined list.
fn get_kargs_in_root(d: &Dir, sys_arch: &str) -> Result<Vec<String>> {
    // If the directory doesn't exist, that's OK.
    let d = if let Some(d) = d.open_dir_optional("usr/lib/bootc/kargs.d")? {
        d
    } else {
        return Ok(Default::default());
    };
    let mut ret = Vec::new();
    // Read all the entries
    let mut entries = d.entries()?.collect::<std::io::Result<Vec<_>>>()?;
    // cc https://github.com/rust-lang/rust/issues/85573 re the allocation-per-comparison here
    entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    for ent in entries {
        let name = ent.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid non-UTF8 filename: {name:?}"))?;
        if !matches!(Utf8Path::new(name).extension(), Some("toml")) {
            continue;
        }
        let buf = d.read_to_string(name)?;
        let kargs = parse_kargs_toml(&buf, sys_arch).with_context(|| format!("Parsing {name}"))?;
        ret.extend(kargs)
    }
    Ok(ret)
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
    let sys_arch = std::env::consts::ARCH;

    // Get the running kargs of the booted system
    if let Some(bootconfig) = ostree::Deployment::bootconfig(booted_deployment) {
        if let Some(options) = ostree::BootconfigParser::get(&bootconfig, "options") {
            let options = options.split_whitespace().map(|s| s.to_owned());
            kargs.extend(options);
        }
    };

    // Get the kargs in kargs.d of the booted system
    let root = &cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    let existing_kargs: Vec<String> = get_kargs_in_root(root, sys_arch)?;

    // Get the kargs in kargs.d of the pending image
    let (fetched_tree, _) = repo.read_commit(fetched.ostree_commit.as_str(), cancellable)?;
    let fetched_tree = fetched_tree.resolve_relative_path("/usr/lib/bootc/kargs.d");
    let fetched_tree = fetched_tree
        .downcast::<ostree::RepoFile>()
        .expect("downcast");
    // A special case: if there's no kargs.d directory in the pending (fetched) image,
    // then we can just use the combined current kargs + kargs from booted
    if !fetched_tree.query_exists(cancellable) {
        kargs.extend(existing_kargs);
        return Ok(kargs);
    }

    let mut remote_kargs: Vec<String> = vec![];
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
                    parse_kargs_toml(&s, sys_arch).with_context(|| format!("Parsing {name}"))?;
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
    let de: Config = toml::from_str(contents)?;
    // if arch specified, apply kargs only if the arch matches
    // if arch not specified, apply kargs unconditionally
    let matched = de
        .match_architectures
        .map(|arches| arches.iter().any(|s| s == sys_arch))
        .unwrap_or(true);
    let r = if matched { de.kargs } else { Vec::new() };
    Ok(r)
}

#[test]
/// Verify that kargs are only applied to supported architectures
fn test_arch() {
    // no arch specified, kargs ensure that kargs are applied unconditionally
    let sys_arch = "x86_64";
    let file_content = r##"kargs = ["console=tty0", "nosmt"]"##.to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);
    let sys_arch = "aarch64";
    let parsed_kargs = parse_kargs_toml(&file_content, sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);

    // one arch matches and one doesn't, ensure that kargs are only applied for the matching arch
    let sys_arch = "aarch64";
    let file_content = r##"kargs = ["console=tty0", "nosmt"]
match-architectures = ["x86_64"]
"##
    .to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, sys_arch).unwrap();
    assert_eq!(parsed_kargs, [] as [String; 0]);
    let file_content = r##"kargs = ["console=tty0", "nosmt"]
match-architectures = ["aarch64"]
"##
    .to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);

    // multiple arch specified, ensure that kargs are applied to both archs
    let sys_arch = "x86_64";
    let file_content = r##"kargs = ["console=tty0", "nosmt"]
match-architectures = ["x86_64", "aarch64"]
"##
    .to_string();
    let parsed_kargs = parse_kargs_toml(&file_content, sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);
    std::env::set_var("ARCH", "aarch64");
    let parsed_kargs = parse_kargs_toml(&file_content, sys_arch).unwrap();
    assert_eq!(parsed_kargs, ["console=tty0", "nosmt"]);
}

#[test]
/// Verify some error cases
fn test_invalid() {
    let test_invalid_extra = r#"kargs = ["console=tty0", "nosmt"]\nfoo=bar"#;
    assert!(parse_kargs_toml(test_invalid_extra, "x86_64").is_err());

    let test_missing = r#"foo=bar"#;
    assert!(parse_kargs_toml(test_missing, "x86_64").is_err());
}

#[test]
fn test_get_kargs_in_root() -> Result<()> {
    let td = cap_std_ext::cap_tempfile::TempDir::new(cap_std::ambient_authority())?;

    // No directory
    assert_eq!(get_kargs_in_root(&td, "x86_64").unwrap().len(), 0);
    // Empty directory
    td.create_dir_all("usr/lib/bootc/kargs.d")?;
    assert_eq!(get_kargs_in_root(&td, "x86_64").unwrap().len(), 0);
    // Non-toml file
    td.write("usr/lib/bootc/kargs.d/somegarbage", "garbage")?;
    assert_eq!(get_kargs_in_root(&td, "x86_64").unwrap().len(), 0);
    td.write(
        "usr/lib/bootc/kargs.d/01-foo.toml",
        r##"kargs = ["console=tty0", "nosmt"]"##,
    )?;
    td.write(
        "usr/lib/bootc/kargs.d/02-bar.toml",
        r##"kargs = ["console=ttyS1"]"##,
    )?;

    let args = get_kargs_in_root(&td, "x86_64").unwrap();
    similar_asserts::assert_eq!(args, ["console=tty0", "nosmt", "console=ttyS1"]);

    Ok(())
}
