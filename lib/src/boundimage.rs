use crate::task::Task;
use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;
use fn_error_context::context;
use ostree_ext::ostree::Deployment;
use ostree_ext::sysroot::SysrootLock;
use rustix::fd::BorrowedFd;

const BOUND_IMAGE_DIR: &str = "usr/lib/bootc-experimental/bound-images.d";

// Access the file descriptor for a sysroot
#[allow(unsafe_code)]
pub(crate) fn sysroot_fd(sysroot: &ostree_ext::ostree::Sysroot) -> BorrowedFd {
    unsafe { BorrowedFd::borrow_raw(sysroot.fd()) }
}

pub(crate) fn pull_bound_images(sysroot: &SysrootLock, deployment: &Deployment) -> Result<()> {
    let sysroot_fd = sysroot_fd(&sysroot);
    let sysroot_fd = Dir::reopen_dir(&sysroot_fd)?;
    let deployment_root_path = sysroot.deployment_dirpath(&deployment);
    let deployment_root = &sysroot_fd.open_dir(&deployment_root_path)?;

    let bound_images = parse_spec_dir(&deployment_root, BOUND_IMAGE_DIR)?;
    pull_images(deployment_root, bound_images)?;

    Ok(())
}

#[context("parse bound image spec dir")]
fn parse_spec_dir(root: &Dir, spec_dir: &str) -> Result<Vec<BoundImage>> {
    let Some(bound_images_dir) = root.open_dir_optional(spec_dir)? else {
        return Ok(Default::default());
    };
    // And open a view of the dir that uses RESOLVE_IN_ROOT so we
    // handle absolute symlinks.
    let absroot = &root.open_dir_rooted_ext(".")?;

    let mut bound_images = Vec::new();

    for entry in bound_images_dir
        .entries()
        .context("Unable to read entries")?
    {
        //validate entry is a symlink with correct extension
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = if let Some(n) = file_name.to_str() {
            n
        } else {
            anyhow::bail!("Invalid non-UTF8 filename: {file_name:?} in {}", spec_dir);
        };

        if !entry.file_type()?.is_symlink() {
            anyhow::bail!("Not a symlink: {file_name}");
        }

        //parse the file contents
        let path = Utf8Path::new(spec_dir).join(file_name);
        let file_contents = absroot.read_to_string(&path)?;

        let file_ini = tini::Ini::from_string(&file_contents).context("Parse to ini")?;
        let file_extension = Utf8Path::new(file_name).extension();
        let bound_image = match file_extension {
            Some("image") => parse_image_file(&file_ini).with_context(|| format!("Parsing {path}")),
            Some("container") => {
                parse_container_file(&file_ini).with_context(|| format!("Parsing {path}"))
            }
            _ => anyhow::bail!("Invalid file extension: {file_name}"),
        }?;

        bound_images.push(bound_image);
    }

    Ok(bound_images)
}

fn parse_image_file(file_contents: &tini::Ini) -> Result<BoundImage> {
    let image: String = file_contents
        .get("Image", "Image")
        .ok_or_else(|| anyhow::anyhow!("Missing Image field"))?;

    //TODO: auth_files have some semi-complicated edge cases that we need to handle,
    //      so for now let's bail out if we see one since the existence of an authfile
    //      will most likely result in a failure to pull the image
    let auth_file: Option<String> = file_contents.get("Image", "AuthFile");
    if auth_file.is_some() {
        anyhow::bail!("AuthFile is not supported by bound bootc images");
    }

    let bound_image = BoundImage::new(image.to_string(), None)?;
    Ok(bound_image)
}

fn parse_container_file(file_contents: &tini::Ini) -> Result<BoundImage> {
    let image: String = file_contents
        .get("Container", "Image")
        .ok_or_else(|| anyhow::anyhow!("Missing Image field"))?;

    let bound_image = BoundImage::new(image.to_string(), None)?;
    Ok(bound_image)
}

#[context("pull bound images")]
fn pull_images(_deployment_root: &Dir, bound_images: Vec<BoundImage>) -> Result<()> {
    //TODO: do this in parallel
    for bound_image in bound_images {
        let mut task = Task::new("Pulling bound image", "/usr/bin/podman")
            .arg("pull")
            .arg(&bound_image.image);
        if let Some(auth_file) = &bound_image.auth_file {
            task = task.arg("--authfile").arg(auth_file);
        }
        task.run()?;
    }

    Ok(())
}

#[derive(PartialEq, Eq)]
struct BoundImage {
    image: String,
    auth_file: Option<String>,
}

impl BoundImage {
    fn new(image: String, auth_file: Option<String>) -> Result<BoundImage> {
        let image = parse_spec_value(&image).context("Invalid image value")?;

        let auth_file = if let Some(auth_file) = &auth_file {
            Some(parse_spec_value(auth_file).context("Invalid auth_file value")?)
        } else {
            None
        };

        Ok(BoundImage { image, auth_file })
    }
}

fn parse_spec_value(value: &str) -> Result<String> {
    let mut it = value.chars();
    let mut ret = String::new();
    while let Some(c) = it.next() {
        if c != '%' {
            ret.push(c);
            continue;
        }
        let c = it.next().ok_or_else(|| anyhow::anyhow!("Unterminated %"))?;
        match c {
            '%' => {
                ret.push('%');
            }
            _ => {
                anyhow::bail!("Systemd specifiers are not supported by bound bootc images: {value}")
            }
        }
    }
    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cap_std_ext::cap_std;
    use std::io::Write;

    #[test]
    fn test_parse_spec_dir() -> Result<()> {
        const CONTAINER_IMAGE_DIR: &'static str = "usr/share/containers/systemd";

        // Empty dir should return an empty vector
        let td = &cap_std_ext::cap_tempfile::TempDir::new(cap_std::ambient_authority())?;
        let images = parse_spec_dir(td, &BOUND_IMAGE_DIR).unwrap();
        assert_eq!(images.len(), 0);

        td.create_dir_all(BOUND_IMAGE_DIR).unwrap();
        td.create_dir_all(CONTAINER_IMAGE_DIR).unwrap();
        let images = parse_spec_dir(td, &BOUND_IMAGE_DIR).unwrap();
        assert_eq!(images.len(), 0);

        // Should return BoundImages
        let mut foo_file = td
            .create(format!("{CONTAINER_IMAGE_DIR}/foo.image"))
            .unwrap();
        foo_file.write_all(b"[Image]\n").unwrap();
        foo_file.write_all(b"Image=quay.io/foo/foo:latest").unwrap();
        td.symlink_contents(
            format!("/{CONTAINER_IMAGE_DIR}/foo.image"),
            format!("{BOUND_IMAGE_DIR}/foo.image"),
        )
        .unwrap();

        let mut bar_file = td
            .create(format!("{CONTAINER_IMAGE_DIR}/bar.image"))
            .unwrap();
        bar_file.write_all(b"[Image]\n").unwrap();
        bar_file.write_all(b"Image=quay.io/bar/bar:latest").unwrap();
        td.symlink_contents(
            format!("/{CONTAINER_IMAGE_DIR}/bar.image"),
            format!("{BOUND_IMAGE_DIR}/bar.image"),
        )
        .unwrap();

        let mut images = parse_spec_dir(td, &BOUND_IMAGE_DIR).unwrap();
        images.sort_by(|a, b| a.image.as_str().cmp(&b.image.as_str()));
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].image, "quay.io/bar/bar:latest");
        assert_eq!(images[1].image, "quay.io/foo/foo:latest");

        // Invalid symlink should return an error
        td.symlink("./blah", format!("{BOUND_IMAGE_DIR}/blah.image"))
            .unwrap();
        assert!(parse_spec_dir(td, &BOUND_IMAGE_DIR).is_err());

        // Invalid image contents should return an error
        let mut error_file = td.create("error.image").unwrap();
        error_file.write_all(b"[Image]\n").unwrap();
        td.symlink_contents("/error.image", format!("{BOUND_IMAGE_DIR}/error.image"))
            .unwrap();
        assert!(parse_spec_dir(td, &BOUND_IMAGE_DIR).is_err());

        Ok(())
    }

    #[test]
    fn test_parse_spec_value() -> Result<()> {
        //should parse string with no % characters
        let value = String::from("quay.io/foo/foo:latest");
        assert_eq!(parse_spec_value(&value).unwrap(), value);

        //should parse string with % followed by another %
        let value = String::from("quay.io/foo/%%foo:latest");
        assert_eq!(parse_spec_value(&value).unwrap(), "quay.io/foo/%foo:latest");

        //should parse string with multiple separate %%
        let value = String::from("quay.io/foo/%%foo:%%latest");
        assert_eq!(
            parse_spec_value(&value).unwrap(),
            "quay.io/foo/%foo:%latest"
        );

        //should parse the string with %% at the start or end
        let value = String::from("%%quay.io/foo/foo:latest%%");
        assert_eq!(
            parse_spec_value(&value).unwrap(),
            "%quay.io/foo/foo:latest%"
        );

        //should not return an error with multiple %% in a row
        let value = String::from("quay.io/foo/%%%%foo:latest");
        assert_eq!(
            parse_spec_value(&value).unwrap(),
            "quay.io/foo/%%foo:latest"
        );

        //should return error when % is NOT followed by another %
        let value = String::from("quay.io/foo/%foo:latest");
        assert!(parse_spec_value(&value).is_err());

        //should return an error when %% is followed by a specifier
        let value = String::from("quay.io/foo/%%%foo:latest");
        assert!(parse_spec_value(&value).is_err());

        //should return an error when there are two specifiers
        let value = String::from("quay.io/foo/%f%ooo:latest");
        assert!(parse_spec_value(&value).is_err());

        //should return an error with a specifier at the start
        let value = String::from("%fquay.io/foo/foo:latest");
        assert!(parse_spec_value(&value).is_err());

        //should return an error with a specifier at the end
        let value = String::from("quay.io/foo/foo:latest%f");
        assert!(parse_spec_value(&value).is_err());

        //should return an error with a single % at the end
        let value = String::from("quay.io/foo/foo:latest%");
        assert!(parse_spec_value(&value).is_err());

        Ok(())
    }

    #[test]
    fn test_parse_image_file() -> Result<()> {
        //should return BoundImage when no auth_file is present
        let file_contents =
            tini::Ini::from_string("[Image]\nImage=quay.io/foo/foo:latest").unwrap();
        let bound_image = parse_image_file(&file_contents).unwrap();
        assert_eq!(bound_image.image, "quay.io/foo/foo:latest");
        assert_eq!(bound_image.auth_file, None);

        //should error when auth_file is present
        let file_contents = tini::Ini::from_string(
            "[Image]\nImage=quay.io/foo/foo:latest\nAuthFile=/etc/containers/auth.json",
        )
        .unwrap();
        assert!(parse_image_file(&file_contents).is_err());

        //should return error when missing image field
        let file_contents = tini::Ini::from_string("[Image]\n").unwrap();
        assert!(parse_image_file(&file_contents).is_err());

        Ok(())
    }

    #[test]
    fn test_parse_container_file() -> Result<()> {
        //should return BoundImage
        let file_contents =
            tini::Ini::from_string("[Container]\nImage=quay.io/foo/foo:latest").unwrap();
        let bound_image = parse_container_file(&file_contents).unwrap();
        assert_eq!(bound_image.image, "quay.io/foo/foo:latest");
        assert_eq!(bound_image.auth_file, None);

        //should return error when missing image field
        let file_contents = tini::Ini::from_string("[Container]\n").unwrap();
        assert!(parse_container_file(&file_contents).is_err());

        Ok(())
    }
}
