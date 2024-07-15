use crate::task::Task;
use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std_ext::cap_std::fs::Dir;
use cap_std_ext::dirext::CapStdExtDirExt;
use fn_error_context::context;
use ostree_ext::ostree::Deployment;
use ostree_ext::sysroot::SysrootLock;
use rustix::fd::BorrowedFd;
use rustix::fs::{OFlags, ResolveFlags};
use std::fs::File;
use std::io::Read;
use std::os::unix::io::AsFd;

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
        let mut file: File = rustix::fs::openat2(
            root.as_fd(),
            path.as_std_path(),
            OFlags::CLOEXEC | OFlags::RDONLY,
            rustix::fs::Mode::empty(),
            ResolveFlags::IN_ROOT,
        )
        .context("Unable to openat")?
        .into();

        let mut file_contents = String::new();
        file.read_to_string(&mut file_contents)
            .context("Unable to read file contents")?;

        let file_ini = tini::Ini::from_string(&file_contents).context("Parse to ini")?;
        let file_extension = Utf8Path::new(file_name).extension();
        let bound_image = match file_extension {
            Some("image") => parse_image_file(file_name, &file_ini),
            Some("container") => parse_container_file(file_name, &file_ini),
            _ => anyhow::bail!("Invalid file extension: {file_name}"),
        }?;

        bound_images.push(bound_image);
    }

    Ok(bound_images)
}

#[context("parse image file {file_name}")]
fn parse_image_file(file_name: &str, file_contents: &tini::Ini) -> Result<BoundImage> {
    let image: String = file_contents
        .get("Image", "Image")
        .ok_or_else(|| anyhow::anyhow!("Missing Image field in {file_name}"))?;

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

#[context("parse container file {file_name}")]
fn parse_container_file(file_name: &str, file_contents: &tini::Ini) -> Result<BoundImage> {
    let image: String = file_contents
        .get("Container", "Image")
        .ok_or_else(|| anyhow::anyhow!("Missing Image field in {file_name}"))?;

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
        validate_spec_value(&image).context("Invalid image value")?;

        if let Some(auth_file) = &auth_file {
            validate_spec_value(auth_file).context("Invalid auth_file value")?;
        }

        Ok(BoundImage { image, auth_file })
    }
}

fn validate_spec_value(value: &String) -> Result<()> {
    let mut number_of_percents = 0;
    for char in value.chars() {
        if char == '%' {
            number_of_percents += 1;
        } else if number_of_percents % 2 != 0 {
            anyhow::bail!("Systemd specifiers are not supported by bound bootc images: {value}");
        } else {
            number_of_percents = 0;
        }
    }

    Ok(())
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
    fn test_validate_spec_value() -> Result<()> {
        //should not return an error with no % characters
        let value = String::from("[Image]\nImage=quay.io/foo/foo:latest");
        validate_spec_value(&value).unwrap();

        //should return error when % is NOT followed by another %
        let value = String::from("[Image]\nImage=quay.io/foo/%foo:latest");
        assert!(validate_spec_value(&value).is_err());

        //should not return an error when % is followed by another %
        let value = String::from("[Image]\nImage=quay.io/foo/%%foo:latest");
        validate_spec_value(&value).unwrap();

        //should not return an error when %% is followed by a specifier
        let value = String::from("[Image]\nImage=quay.io/foo/%%%foo:latest");
        assert!(validate_spec_value(&value).is_err());

        Ok(())
    }

    #[test]
    fn test_parse_image_file() -> Result<()> {
        //should return BoundImage when no auth_file is present
        let file_contents =
            tini::Ini::from_string("[Image]\nImage=quay.io/foo/foo:latest").unwrap();
        let bound_image = parse_image_file("foo.image", &file_contents).unwrap();
        assert_eq!(bound_image.image, "quay.io/foo/foo:latest");
        assert_eq!(bound_image.auth_file, None);

        //should error when auth_file is present
        let file_contents = tini::Ini::from_string(
            "[Image]\nImage=quay.io/foo/foo:latest\nAuthFile=/etc/containers/auth.json",
        )
        .unwrap();
        assert!(parse_image_file("foo.image", &file_contents).is_err());

        //should return error when missing image field
        let file_contents = tini::Ini::from_string("[Image]\n").unwrap();
        assert!(parse_image_file("foo.image", &file_contents).is_err());

        Ok(())
    }

    #[test]
    fn test_parse_container_file() -> Result<()> {
        //should return BoundImage
        let file_contents =
            tini::Ini::from_string("[Container]\nImage=quay.io/foo/foo:latest").unwrap();
        let bound_image = parse_container_file("foo.container", &file_contents).unwrap();
        assert_eq!(bound_image.image, "quay.io/foo/foo:latest");
        assert_eq!(bound_image.auth_file, None);

        //should return error when missing image field
        let file_contents = tini::Ini::from_string("[Container]\n").unwrap();
        assert!(parse_container_file("foo.container", &file_contents).is_err());

        Ok(())
    }
}
