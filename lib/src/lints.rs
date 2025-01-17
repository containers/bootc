//! # Implementation of container build lints
//!
//! This module implements `bootc container lint`.

use std::env::consts::ARCH;
use std::os::unix::ffi::OsStrExt;

use anyhow::{Context, Result};
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use cap_std_ext::dirext::CapStdExtDirExt as _;
use fn_error_context::context;

/// Reference to embedded default baseimage content that should exist.
const BASEIMAGE_REF: &str = "usr/share/doc/bootc/baseimage/base";

/// A lint check has failed.
#[derive(thiserror::Error, Debug)]
struct LintError(String);

/// The outer error is for unexpected fatal runtime problems; the
/// inner error is for the lint failing in an expected way.
type LintResult = Result<std::result::Result<(), LintError>>;

/// Everything is OK - we didn't encounter a runtime error, and
/// the targeted check passed.
fn lint_ok() -> LintResult {
    Ok(Ok(()))
}

/// We successfully found a lint failure.
fn lint_err(msg: impl AsRef<str>) -> LintResult {
    Ok(Err(LintError::new(msg)))
}

impl std::fmt::Display for LintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl LintError {
    fn new(msg: impl AsRef<str>) -> Self {
        Self(msg.as_ref().to_owned())
    }
}

type LintFn = fn(&Dir) -> LintResult;

/// The classification of a lint type.
#[derive(Debug)]
enum LintType {
    /// If this fails, it is known to be fatal - the system will not install or
    /// is effectively guaranteed to fail at runtime.
    Fatal,
}

struct Lint {
    name: &'static str,
    ty: LintType,
    f: LintFn,
}

const LINTS: &[Lint] = &[
    Lint {
        name: "var-run",
        ty: LintType::Fatal,
        f: check_var_run,
    },
    Lint {
        name: "kernel",
        ty: LintType::Fatal,
        f: check_kernel,
    },
    Lint {
        name: "bootc-kargs",
        ty: LintType::Fatal,
        f: check_parse_kargs,
    },
    Lint {
        name: "etc-usretc",
        ty: LintType::Fatal,
        f: check_usretc,
    },
    Lint {
        // This one can be lifted in the future, see https://github.com/containers/bootc/issues/975
        name: "utf8",
        ty: LintType::Fatal,
        f: check_utf8,
    },
    Lint {
        name: "baseimage-root",
        ty: LintType::Fatal,
        f: check_baseimage_root,
    },
];

/// check for the existence of the /var/run directory
/// if it exists we need to check that it links to /run if not error
/// if it does not exist error.
#[context("Linting")]
pub(crate) fn lint(root: &Dir) -> Result<()> {
    let mut fatal = 0usize;
    let mut passed = 0usize;
    for lint in LINTS {
        let name = lint.name;
        let r = match (lint.f)(&root) {
            Ok(r) => r,
            Err(e) => anyhow::bail!("Unexpected runtime error running lint {name}: {e}"),
        };
        if let Err(e) = r {
            eprintln!("Failed lint: {name}: {e}");
            fatal += 1;
        } else {
            // We'll be quiet for now
            tracing::debug!("OK {name} (type={:?})", lint.ty);
            passed += 1;
        }
    }
    println!("Checks passed: {passed}");
    if fatal > 0 {
        anyhow::bail!("Checks failed: {fatal}")
    }
    Ok(())
}

fn check_var_run(root: &Dir) -> LintResult {
    if let Some(meta) = root.symlink_metadata_optional("var/run")? {
        if !meta.is_symlink() {
            return lint_err("Not a symlink: var/run");
        }
    }
    lint_ok()
}

fn check_usretc(root: &Dir) -> LintResult {
    let etc_exists = root.symlink_metadata_optional("etc")?.is_some();
    // For compatibility/conservatism don't bomb out if there's no /etc.
    if !etc_exists {
        return lint_ok();
    }
    // But having both /etc and /usr/etc is not something we want to support.
    if root.symlink_metadata_optional("usr/etc")?.is_some() {
        return lint_err(
            "Found /usr/etc - this is a bootc implementation detail and not supported to use in containers"
        );
    }
    lint_ok()
}

/// Validate that we can parse the /usr/lib/bootc/kargs.d files.
fn check_parse_kargs(root: &Dir) -> LintResult {
    let args = crate::kargs::get_kargs_in_root(root, ARCH)?;
    tracing::debug!("found kargs: {args:?}");
    lint_ok()
}

fn check_kernel(root: &Dir) -> LintResult {
    let result = ostree_ext::bootabletree::find_kernel_dir_fs(&root)?;
    tracing::debug!("Found kernel: {:?}", result);
    lint_ok()
}

fn check_utf8(dir: &Dir) -> LintResult {
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();

        let Some(strname) = &name.to_str() else {
            // will escape nicely like "abc\xFFdéf"
            return lint_err(format!("/: Found non-utf8 filename {name:?}"));
        };

        let ifmt = entry.file_type()?;
        if ifmt.is_symlink() {
            let target = dir.read_link_contents(&name)?;
            if !target.to_str().is_some() {
                return lint_err(format!("/{strname}: Found non-utf8 symlink target"));
            }
        } else if ifmt.is_dir() {
            let Some(subdir) = crate::utils::open_dir_noxdev(dir, entry.file_name())? else {
                continue;
            };
            if let Err(err) = check_utf8(&subdir)? {
                // Try to do the path pasting only in the event of an error
                return lint_err(format!("/{strname}{err}"));
            }
        }
    }
    lint_ok()
}

/// Check for a few files and directories we expect in the base image.
fn check_baseimage_root_norecurse(dir: &Dir) -> LintResult {
    // Check /sysroot
    let meta = dir.symlink_metadata_optional("sysroot")?;
    match meta {
        Some(meta) if !meta.is_dir() => return lint_err("Expected a directory for /sysroot"),
        None => return lint_err("Missing /sysroot"),
        _ => {}
    }

    // Check /ostree -> sysroot/ostree
    let Some(meta) = dir.symlink_metadata_optional("ostree")? else {
        return lint_err("Missing ostree -> sysroot/ostree link");
    };
    if !meta.is_symlink() {
        return lint_err("/ostree should be a symlink");
    }
    let link = dir.read_link_contents("ostree")?;
    let expected = "sysroot/ostree";
    if link.as_os_str().as_bytes() != expected.as_bytes() {
        return lint_err("Expected /ostree -> {expected}, not {link:?}");
    }

    // Check the prepare-root config
    let prepareroot_path = "usr/lib/ostree/prepare-root.conf";
    let config_data = dir
        .read_to_string(prepareroot_path)
        .context(prepareroot_path)?;
    let config = ostree_ext::glib::KeyFile::new();
    config.load_from_data(&config_data, ostree_ext::glib::KeyFileFlags::empty())?;

    if !ostree_ext::ostree_prepareroot::overlayfs_enabled_in_config(&config)? {
        return lint_err("{prepareroot_path} does not have composefs enabled");
    }

    lint_ok()
}

/// Check ostree-related base image content.
fn check_baseimage_root(dir: &Dir) -> LintResult {
    check_baseimage_root_norecurse(dir)??;
    // If we have our own documentation with the expected root contents
    // embedded, then check that too! Mostly just because recursion is fun.
    if let Some(dir) = dir.open_dir_optional(BASEIMAGE_REF)? {
        check_baseimage_root_norecurse(&dir)??;
    }
    lint_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Result<cap_std_ext::cap_tempfile::TempDir> {
        let tempdir = cap_std_ext::cap_tempfile::tempdir(cap_std::ambient_authority())?;
        Ok(tempdir)
    }

    #[test]
    fn test_var_run() -> Result<()> {
        let root = &fixture()?;
        // This one should pass
        check_var_run(root).unwrap().unwrap();
        root.create_dir_all("var/run/foo")?;
        assert!(check_var_run(root).unwrap().is_err());
        root.remove_dir_all("var/run")?;
        // Now we should pass again
        check_var_run(root).unwrap().unwrap();
        Ok(())
    }

    #[test]
    fn test_kernel_lint() -> Result<()> {
        let root = &fixture()?;
        // This one should pass
        check_kernel(root).unwrap().unwrap();
        root.create_dir_all("usr/lib/modules/5.7.2")?;
        root.write("usr/lib/modules/5.7.2/vmlinuz", "old vmlinuz")?;
        root.create_dir_all("usr/lib/modules/6.3.1")?;
        root.write("usr/lib/modules/6.3.1/vmlinuz", "new vmlinuz")?;
        assert!(check_kernel(root).is_err());
        root.remove_dir_all("usr/lib/modules/5.7.2")?;
        // Now we should pass again
        check_kernel(root).unwrap().unwrap();
        Ok(())
    }

    #[test]
    fn test_kargs() -> Result<()> {
        let root = &fixture()?;
        check_parse_kargs(root).unwrap().unwrap();
        root.create_dir_all("usr/lib/bootc")?;
        root.write("usr/lib/bootc/kargs.d", "not a directory")?;
        assert!(check_parse_kargs(root).is_err());
        Ok(())
    }

    #[test]
    fn test_usr_etc() -> Result<()> {
        let root = &fixture()?;
        // This one should pass
        check_usretc(root).unwrap().unwrap();
        root.create_dir_all("etc")?;
        root.create_dir_all("usr/etc")?;
        assert!(check_usretc(root).unwrap().is_err());
        root.remove_dir_all("etc")?;
        // Now we should pass again
        check_usretc(root).unwrap().unwrap();
        Ok(())
    }

    #[test]
    fn test_non_utf8() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let root = &fixture().unwrap();

        // Try to create some adversarial symlink situations to ensure the walk doesn't crash
        root.create_dir("subdir").unwrap();
        // Self-referential symlinks
        root.symlink("self", "self").unwrap();
        // Infinitely looping dir symlinks
        root.symlink("..", "subdir/parent").unwrap();
        // Broken symlinks
        root.symlink("does-not-exist", "broken").unwrap();
        // Out-of-scope symlinks
        root.symlink("../../x", "escape").unwrap();
        // Should be fine
        check_utf8(root).unwrap().unwrap();

        // But this will cause an issue
        let baddir = OsStr::from_bytes(b"subdir/2/bad\xffdir");
        root.create_dir("subdir/2").unwrap();
        root.create_dir(baddir).unwrap();
        let Err(err) = check_utf8(root).unwrap() else {
            unreachable!("Didn't fail");
        };
        assert_eq!(
            err.to_string(),
            r#"/subdir/2/: Found non-utf8 filename "bad\xFFdir""#
        );
        root.remove_dir(baddir).unwrap(); // Get rid of the problem
        check_utf8(root).unwrap().unwrap(); // Check it

        // Create a new problem in the form of a regular file
        let badfile = OsStr::from_bytes(b"regular\xff");
        root.write(badfile, b"Hello, world!\n").unwrap();
        let Err(err) = check_utf8(root).unwrap() else {
            unreachable!("Didn't fail");
        };
        assert_eq!(
            err.to_string(),
            r#"/: Found non-utf8 filename "regular\xFF""#
        );
        root.remove_file(badfile).unwrap(); // Get rid of the problem
        check_utf8(root).unwrap().unwrap(); // Check it

        // And now test invalid symlink targets
        root.symlink(badfile, "subdir/good-name").unwrap();
        let Err(err) = check_utf8(root).unwrap() else {
            unreachable!("Didn't fail");
        };
        assert_eq!(
            err.to_string(),
            r#"/subdir/good-name: Found non-utf8 symlink target"#
        );
        root.remove_file("subdir/good-name").unwrap(); // Get rid of the problem
        check_utf8(root).unwrap().unwrap(); // Check it

        // Finally, test a self-referential symlink with an invalid name.
        // We should spot the invalid name before we check the target.
        root.symlink(badfile, badfile).unwrap();
        let Err(err) = check_utf8(root).unwrap() else {
            unreachable!("Didn't fail");
        };
        assert_eq!(
            err.to_string(),
            r#"/: Found non-utf8 filename "regular\xFF""#
        );
        root.remove_file(badfile).unwrap(); // Get rid of the problem
        check_utf8(root).unwrap().unwrap(); // Check it
    }

    #[test]
    fn test_baseimage_root() -> Result<()> {
        use bootc_utils::CommandRunExt;
        use cap_std_ext::cmdext::CapStdExtCommandExt;
        use std::path::Path;

        let td = fixture()?;

        // An empty root should fail our test
        assert!(check_baseimage_root(&td).is_err());

        // Copy our reference base image content from the source dir
        let Some(manifest) = std::env::var_os("CARGO_MANIFEST_PATH") else {
            // This was only added in relatively recent cargo
            return Ok(());
        };
        let srcdir = Path::new(&manifest)
            .parent()
            .unwrap()
            .join("../baseimage/base");
        for ent in std::fs::read_dir(srcdir)? {
            let ent = ent?;
            std::process::Command::new("cp")
                .cwd_dir(td.try_clone()?)
                .arg("-pr")
                .arg(ent.path())
                .arg(".")
                .run()?;
        }
        check_baseimage_root(&td).unwrap().unwrap();
        Ok(())
    }
}
