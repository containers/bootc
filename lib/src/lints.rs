//! # Implementation of container build lints
//!
//! This module implements `bootc container lint`.

use std::env::consts::ARCH;

use anyhow::{bail, ensure, Result};
use cap_std::fs::Dir;
use cap_std_ext::cap_std;
use cap_std_ext::dirext::CapStdExtDirExt as _;
use fn_error_context::context;

use crate::utils::openat2_with_retry;

/// check for the existence of the /var/run directory
/// if it exists we need to check that it links to /run if not error
/// if it does not exist error.
#[context("Linting")]
pub(crate) fn lint(root: &Dir) -> Result<()> {
    let lints = [
        check_var_run,
        check_kernel,
        check_parse_kargs,
        check_usretc,
        check_utf8,
    ];
    for lint in lints {
        lint(&root)?;
    }
    println!("Checks passed: {}", lints.len());
    Ok(())
}

fn check_var_run(root: &Dir) -> Result<()> {
    if let Some(meta) = root.symlink_metadata_optional("var/run")? {
        if !meta.is_symlink() {
            anyhow::bail!("Not a symlink: var/run");
        }
    }
    Ok(())
}

fn check_usretc(root: &Dir) -> Result<()> {
    let etc_exists = root.symlink_metadata_optional("etc")?.is_some();
    // For compatibility/conservatism don't bomb out if there's no /etc.
    if !etc_exists {
        return Ok(());
    }
    // But having both /etc and /usr/etc is not something we want to support.
    if root.symlink_metadata_optional("usr/etc")?.is_some() {
        anyhow::bail!(
            "Found /usr/etc - this is a bootc implementation detail and not supported to use in containers"
        );
    }
    Ok(())
}

/// Validate that we can parse the /usr/lib/bootc/kargs.d files.
fn check_parse_kargs(root: &Dir) -> Result<()> {
    let _args = crate::kargs::get_kargs_in_root(root, ARCH)?;
    Ok(())
}

fn check_kernel(root: &Dir) -> Result<()> {
    let result = ostree_ext::bootabletree::find_kernel_dir_fs(&root)?;
    tracing::debug!("Found kernel: {:?}", result);
    Ok(())
}

/// Open the target directory, but return Ok(None) if this would cross a mount point.
fn open_dir_noxdev(
    parent: &Dir,
    path: impl AsRef<std::path::Path>,
) -> std::io::Result<Option<Dir>> {
    use rustix::fs::{Mode, OFlags, ResolveFlags};
    match openat2_with_retry(
        parent,
        path,
        OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
        ResolveFlags::NO_XDEV | ResolveFlags::BENEATH,
    ) {
        Ok(r) => Ok(Some(Dir::reopen_dir(&r)?)),
        Err(e) if e == rustix::io::Errno::XDEV => Ok(None),
        Err(e) => return Err(e.into()),
    }
}

fn check_utf8(dir: &Dir) -> Result<()> {
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();

        let Some(strname) = &name.to_str() else {
            // will escape nicely like "abc\xFFdéf"
            bail!("/: Found non-utf8 filename {name:?}");
        };

        let ifmt = entry.file_type()?;
        if ifmt.is_symlink() {
            let target = dir.read_link_contents(&name)?;
            ensure!(
                target.to_str().is_some(),
                "/{strname}: Found non-utf8 symlink target"
            );
        } else if ifmt.is_dir() {
            let Some(subdir) = open_dir_noxdev(dir, entry.file_name())? else {
                continue;
            };
            if let Err(err) = check_utf8(&subdir) {
                // Try to do the path pasting only in the event of an error
                bail!("/{strname}{err:?}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn fixture() -> Result<cap_std_ext::cap_tempfile::TempDir> {
    let tempdir = cap_std_ext::cap_tempfile::tempdir(cap_std::ambient_authority())?;
    Ok(tempdir)
}

#[test]
fn test_open_noxdev() -> Result<()> {
    let root = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    // This hard requires the host setup to have /usr/bin on the same filesystem as /
    let usr = Dir::open_ambient_dir("/usr", cap_std::ambient_authority())?;
    assert!(open_dir_noxdev(&usr, "bin").unwrap().is_some());
    // Requires a mounted /proc, but that also seems ane.
    assert!(open_dir_noxdev(&root, "proc").unwrap().is_none());
    Ok(())
}

#[test]
fn test_var_run() -> Result<()> {
    let root = &fixture()?;
    // This one should pass
    check_var_run(root).unwrap();
    root.create_dir_all("var/run/foo")?;
    assert!(check_var_run(root).is_err());
    root.remove_dir_all("var/run")?;
    // Now we should pass again
    check_var_run(root).unwrap();
    Ok(())
}

#[test]
fn test_kernel_lint() -> Result<()> {
    let root = &fixture()?;
    // This one should pass
    check_kernel(root).unwrap();
    root.create_dir_all("usr/lib/modules/5.7.2")?;
    root.write("usr/lib/modules/5.7.2/vmlinuz", "old vmlinuz")?;
    root.create_dir_all("usr/lib/modules/6.3.1")?;
    root.write("usr/lib/modules/6.3.1/vmlinuz", "new vmlinuz")?;
    assert!(check_kernel(root).is_err());
    root.remove_dir_all("usr/lib/modules/5.7.2")?;
    // Now we should pass again
    check_kernel(root).unwrap();
    Ok(())
}

#[test]
fn test_kargs() -> Result<()> {
    let root = &fixture()?;
    check_parse_kargs(root).unwrap();
    root.create_dir_all("usr/lib/bootc")?;
    root.write("usr/lib/bootc/kargs.d", "not a directory")?;
    assert!(check_parse_kargs(root).is_err());
    Ok(())
}

#[test]
fn test_usr_etc() -> Result<()> {
    let root = &fixture()?;
    // This one should pass
    check_usretc(root).unwrap();
    root.create_dir_all("etc")?;
    root.create_dir_all("usr/etc")?;
    assert!(check_usretc(root).is_err());
    root.remove_dir_all("etc")?;
    // Now we should pass again
    check_usretc(root).unwrap();
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
    check_utf8(root).unwrap();

    // But this will cause an issue
    let baddir = OsStr::from_bytes(b"subdir/2/bad\xffdir");
    root.create_dir("subdir/2").unwrap();
    root.create_dir(baddir).unwrap();
    let Err(err) = check_utf8(root) else {
        unreachable!("Didn't fail");
    };
    assert_eq!(
        err.to_string(),
        r#"/subdir/2/: Found non-utf8 filename "bad\xFFdir""#
    );
    root.remove_dir(baddir).unwrap(); // Get rid of the problem
    check_utf8(root).unwrap(); // Check it

    // Create a new problem in the form of a regular file
    let badfile = OsStr::from_bytes(b"regular\xff");
    root.write(badfile, b"Hello, world!\n").unwrap();
    let Err(err) = check_utf8(root) else {
        unreachable!("Didn't fail");
    };
    assert_eq!(
        err.to_string(),
        r#"/: Found non-utf8 filename "regular\xFF""#
    );
    root.remove_file(badfile).unwrap(); // Get rid of the problem
    check_utf8(root).unwrap(); // Check it

    // And now test invalid symlink targets
    root.symlink(badfile, "subdir/good-name").unwrap();
    let Err(err) = check_utf8(root) else {
        unreachable!("Didn't fail");
    };
    assert_eq!(
        err.to_string(),
        r#"/subdir/good-name: Found non-utf8 symlink target"#
    );
    root.remove_file("subdir/good-name").unwrap(); // Get rid of the problem
    check_utf8(root).unwrap(); // Check it

    // Finally, test a self-referential symlink with an invalid name.
    // We should spot the invalid name before we check the target.
    root.symlink(badfile, badfile).unwrap();
    let Err(err) = check_utf8(root) else {
        unreachable!("Didn't fail");
    };
    assert_eq!(
        err.to_string(),
        r#"/: Found non-utf8 filename "regular\xFF""#
    );
    root.remove_file(badfile).unwrap(); // Get rid of the problem
    check_utf8(root).unwrap(); // Check it
}
