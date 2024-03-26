use std::fmt::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use fn_error_context::context;
use openat_ext::OpenatDirExt;

/// The subdirectory of /boot we use
const GRUB2DIR: &str = "grub2";
const CONFIGDIR: &str = "/usr/lib/bootupd/grub2-static";
const DROPINDIR: &str = "configs.d";

#[context("Locating EFI vendordir")]
pub(crate) fn find_efi_vendordir(efidir: &openat::Dir) -> Result<PathBuf> {
    for d in efidir.list_dir(".")? {
        let d = d?;
        let meta = efidir.metadata(d.file_name())?;
        if !meta.is_dir() {
            continue;
        }
        // skip if not find shim under dir
        let dir = efidir.sub_dir(d.file_name())?;
        for entry in dir.list_dir(".")? {
            let entry = entry?;
            if entry.file_name() != super::efi::SHIM {
                continue;
            }
            return Ok(d.file_name().into());
        }
    }
    anyhow::bail!("Failed to find EFI vendor dir")
}

/// Install the static GRUB config files.
#[context("Installing static GRUB configs")]
pub(crate) fn install(target_root: &openat::Dir, efi: bool, write_uuid: bool) -> Result<()> {
    let bootdir = &target_root.sub_dir("boot").context("Opening /boot")?;
    let boot_is_mount = {
        let root_dev = target_root.self_metadata()?.stat().st_dev;
        let boot_dev = bootdir.self_metadata()?.stat().st_dev;
        log::debug!("root_dev={root_dev} boot_dev={boot_dev}");
        root_dev != boot_dev
    };

    if !bootdir.exists(GRUB2DIR)? {
        bootdir.create_dir(GRUB2DIR, 0o700)?;
    }

    let mut config = std::fs::read_to_string(Path::new(CONFIGDIR).join("grub-static-pre.cfg"))?;

    let dropindir = openat::Dir::open(&Path::new(CONFIGDIR).join(DROPINDIR))?;
    // Sort the files for reproducibility
    let mut entries = dropindir
        .list_dir(".")?
        .map(|e| e.map_err(anyhow::Error::msg))
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by(|a, b| a.file_name().cmp(b.file_name()));
    for ent in entries {
        let name = ent.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("Invalid UTF-8: {name:?}"))?;
        if !name.ends_with(".cfg") {
            log::debug!("Ignoring {name}");
            continue;
        }
        writeln!(config, "source $prefix/{name}")?;
        dropindir
            .copy_file_at(name, bootdir, format!("{GRUB2DIR}/{name}"))
            .with_context(|| format!("Copying {name}"))?;
        println!("Installed {name}");
    }

    {
        let post = std::fs::read_to_string(Path::new(CONFIGDIR).join("grub-static-post.cfg"))?;
        config.push_str(post.as_str());
    }

    bootdir
        .write_file_contents(format!("{GRUB2DIR}/grub.cfg"), 0o644, config.as_bytes())
        .context("Copying grub-static.cfg")?;
    println!("Installed: grub.cfg");

    let uuid_path = if write_uuid {
        let target_fs = if boot_is_mount { bootdir } else { target_root };
        let bootfs_meta = crate::filesystem::inspect_filesystem(target_fs, ".")?;
        let bootfs_uuid = bootfs_meta
            .uuid
            .ok_or_else(|| anyhow::anyhow!("Failed to find UUID for boot"))?;
        let grub2_uuid_contents = format!("set BOOT_UUID=\"{bootfs_uuid}\"\n");
        let uuid_path = format!("{GRUB2DIR}/bootuuid.cfg");
        bootdir
            .write_file_contents(&uuid_path, 0o644, grub2_uuid_contents)
            .context("Writing bootuuid.cfg")?;
        Some(uuid_path)
    } else {
        None
    };

    let efidir = efi
        .then(|| {
            target_root
                .sub_dir_optional("boot/efi/EFI")
                .context("Opening /boot/efi/EFI")
        })
        .transpose()?
        .flatten();
    if let Some(efidir) = efidir.as_ref() {
        let vendordir = find_efi_vendordir(efidir)?;
        log::debug!("vendordir={:?}", &vendordir);
        let target = &vendordir.join("grub.cfg");
        efidir
            .copy_file(&Path::new(CONFIGDIR).join("grub-static-efi.cfg"), target)
            .context("Copying static EFI")?;
        println!("Installed: {target:?}");
        if let Some(uuid_path) = uuid_path {
            // SAFETY: we always have a filename
            let filename = Path::new(&uuid_path).file_name().unwrap();
            let target = &vendordir.join(filename);
            bootdir
                .copy_file_at(uuid_path, efidir, target)
                .context("Writing bootuuid.cfg to efi dir")?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn test_install() -> Result<()> {
        env_logger::init();
        let td = tempfile::tempdir()?;
        let tdp = td.path();
        let td = openat::Dir::open(tdp)?;
        std::fs::create_dir_all(tdp.join("boot/grub2"))?;
        std::fs::create_dir_all(tdp.join("boot/efi/EFI/BOOT"))?;
        std::fs::create_dir_all(tdp.join("boot/efi/EFI/fedora"))?;
        install(&td, true, false).unwrap();

        assert!(td.exists("boot/grub2/grub.cfg")?);
        assert!(td.exists("boot/efi/EFI/fedora/grub.cfg")?);
        Ok(())
    }

    #[test]
    fn test_find_efi_vendordir() -> Result<()> {
        let td = tempfile::tempdir()?;
        let tdp = td.path();
        let efidir = tdp.join("EFI");
        std::fs::create_dir_all(efidir.join("BOOT"))?;
        std::fs::create_dir_all(efidir.join("dell"))?;
        std::fs::create_dir_all(efidir.join("fedora"))?;
        let td = openat::Dir::open(&efidir)?;

        std::fs::write(efidir.join("dell").join("foo"), "foo data")?;
        std::fs::write(efidir.join("fedora").join("grub.cfg"), "grub config")?;
        std::fs::write(efidir.join("fedora").join("shimx64.efi"), "shim data")?;

        assert!(td.exists("BOOT")?);
        assert!(td.exists("dell/foo")?);
        assert!(td.exists("fedora/grub.cfg")?);
        assert!(td.exists("fedora/shimx64.efi")?);
        assert_eq!(find_efi_vendordir(&td)?.to_str(), Some("fedora"));

        std::fs::remove_file(efidir.join("fedora").join("shimx64.efi"))?;
        let x = find_efi_vendordir(&td);
        assert_eq!(x.is_err(), true);
        Ok(())
    }
}
