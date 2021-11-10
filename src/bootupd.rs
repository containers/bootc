use crate::component::{Component, ValidationResult};
use crate::coreos;
use crate::efi;
use crate::model::{ComponentStatus, ComponentUpdatable, ContentMetadata, SavedState, Status};
use crate::util;
use crate::{component, ipc};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

/// A message sent from client to server
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ClientRequest {
    /// Update a component
    Update { component: String },
    /// Update a component via adoption
    AdoptAndUpdate { component: String },
    /// Validate a component
    Validate { component: String },
    /// Print the current state
    Status,
}

pub(crate) fn install(source_root: &str, dest_root: &str) -> Result<()> {
    let source_root = openat::Dir::open(source_root)?;
    SavedState::ensure_not_present(dest_root)
        .context("failed to install, invalid re-install attempted")?;

    let components = get_components();
    if components.is_empty() {
        println!("No components available for this platform.");
        return Ok(());
    }

    let mut state = SavedState::default();
    for component in components.values() {
        let meta = component
            .install(&source_root, dest_root)
            .with_context(|| format!("installing component {}", component.name()))?;
        state.installed.insert(component.name().into(), meta);
    }

    let sysroot = openat::Dir::open(dest_root)?;
    let mut state_guard = SavedState::unlocked(sysroot).context("failed to acquire write lock")?;
    state_guard
        .update_state(&state)
        .context("failed to update state")?;

    Ok(())
}

type Components = BTreeMap<&'static str, Box<dyn Component>>;

pub(crate) fn get_components() -> Components {
    let mut components = BTreeMap::new();

    fn insert_component(components: &mut Components, component: Box<dyn Component>) {
        components.insert(component.name(), component);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    insert_component(&mut components, Box::new(efi::Efi::default()));

    // #[cfg(target_arch = "x86_64")]
    // components.push(Box::new(bios::BIOS::new()));

    components
}

pub(crate) fn generate_update_metadata(sysroot_path: &str) -> Result<()> {
    for component in get_components().values() {
        let v = component.generate_update_metadata(sysroot_path)?;
        println!(
            "Generated update layout for {}: {}",
            component.name(),
            v.version,
        );
    }

    Ok(())
}

/// Return value from daemon → client for component update
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ComponentUpdateResult {
    AtLatestVersion,
    Updated {
        previous: ContentMetadata,
        interrupted: Option<ContentMetadata>,
        new: ContentMetadata,
    },
}

fn ensure_writable_boot() -> Result<()> {
    util::ensure_writable_mount("/boot")
}

/// daemon implementation of component update
pub(crate) fn update(name: &str) -> Result<ComponentUpdateResult> {
    let mut state = SavedState::load_from_disk("/")?.unwrap_or_default();
    let component = component::new_from_name(name)?;
    let inst = if let Some(inst) = state.installed.get(name) {
        inst.clone()
    } else {
        anyhow::bail!("Component {} is not installed", name);
    };
    let sysroot = openat::Dir::open("/")?;
    let update = component.query_update(&sysroot)?;
    let update = match update.as_ref() {
        Some(p) if inst.meta.can_upgrade_to(p) => p,
        _ => return Ok(ComponentUpdateResult::AtLatestVersion),
    };

    ensure_writable_boot()?;

    let mut pending_container = state.pending.take().unwrap_or_default();
    let interrupted = pending_container.get(component.name()).cloned();
    pending_container.insert(component.name().into(), update.clone());
    let mut state_guard =
        SavedState::acquire_write_lock(sysroot).context("Failed to acquire write lock")?;
    state_guard
        .update_state(&state)
        .context("Failed to update state")?;

    let newinst = component
        .run_update(&state_guard.sysroot, &inst)
        .with_context(|| format!("Failed to update {}", component.name()))?;
    state.installed.insert(component.name().into(), newinst);
    pending_container.remove(component.name());
    state_guard.update_state(&state)?;

    Ok(ComponentUpdateResult::Updated {
        previous: inst.meta,
        interrupted,
        new: update.clone(),
    })
}

/// daemon implementation of component adoption
pub(crate) fn adopt_and_update(name: &str) -> Result<ContentMetadata> {
    let sysroot = openat::Dir::open("/")?;
    let mut state = SavedState::load_from_disk("/")?.unwrap_or_default();
    let component = component::new_from_name(name)?;
    if state.installed.get(name).is_some() {
        anyhow::bail!("Component {} is already installed", name);
    };

    ensure_writable_boot()?;

    let update = if let Some(update) = component.query_update(&sysroot)? {
        update
    } else {
        anyhow::bail!("Component {} has no available update", name);
    };
    let mut state_guard =
        SavedState::acquire_write_lock(sysroot).context("Failed to acquire write lock")?;

    let inst = component
        .adopt_update(&state_guard.sysroot, &update)
        .context("Failed adopt and update")?;
    state.installed.insert(component.name().into(), inst);

    state_guard.update_state(&state)?;
    Ok(update)
}

/// daemon implementation of component validate
pub(crate) fn validate(name: &str) -> Result<ValidationResult> {
    let state = SavedState::load_from_disk("/")?.unwrap_or_default();
    let component = component::new_from_name(name)?;
    let inst = if let Some(inst) = state.installed.get(name) {
        inst.clone()
    } else {
        anyhow::bail!("Component {} is not installed", name);
    };
    component.validate(&inst)
}

pub(crate) fn status() -> Result<Status> {
    let mut ret: Status = Default::default();
    let mut known_components = get_components();
    let sysroot = openat::Dir::open("/")?;
    let state = SavedState::load_from_disk("/")?;
    if let Some(state) = state {
        for (name, ic) in state.installed.iter() {
            log::trace!("Gathering status for installed component: {}", name);
            let component = known_components
                .remove(name.as_str())
                .ok_or_else(|| anyhow!("Unknown component installed: {}", name))?;
            let component = component.as_ref();
            let interrupted = state
                .pending
                .as_ref()
                .map(|p| p.get(name.as_str()))
                .flatten();
            let update = component.query_update(&sysroot)?;
            let updatable = ComponentUpdatable::from_metadata(&ic.meta, update.as_ref());
            let adopted_from = ic.adopted_from.clone();
            ret.components.insert(
                name.to_string(),
                ComponentStatus {
                    installed: ic.meta.clone(),
                    interrupted: interrupted.cloned(),
                    update,
                    updatable,
                    adopted_from,
                },
            );
        }
    } else {
        log::trace!("No saved state");
    }

    // Process the remaining components not installed
    log::trace!("Remaining known components: {}", known_components.len());
    for (name, component) in known_components {
        if let Some(adopt_ver) = component.query_adopt()? {
            ret.adoptable.insert(name.to_string(), adopt_ver);
        } else {
            log::trace!("Not adoptable: {}", name);
        }
    }

    Ok(ret)
}

pub(crate) fn print_status_avail(status: &Status) -> Result<()> {
    let mut avail = Vec::new();
    for (name, component) in status.components.iter() {
        if let ComponentUpdatable::Upgradable = component.updatable {
            avail.push(name.as_str());
        }
    }
    for (name, adoptable) in status.adoptable.iter() {
        if adoptable.confident {
            avail.push(name.as_str());
        }
    }
    if !avail.is_empty() {
        println!("Updates available: {}", avail.join(" "));
    }
    Ok(())
}

pub(crate) fn print_status(status: &Status) -> Result<()> {
    if status.components.is_empty() {
        println!("No components installed.");
    }
    for (name, component) in status.components.iter() {
        println!("Component {}", name);
        println!("  Installed: {}", component.installed.version);

        if let Some(i) = component.interrupted.as_ref() {
            println!(
                "  WARNING: Previous update to {} was interrupted",
                i.version
            );
        }
        let msg = match component.updatable {
            ComponentUpdatable::NoUpdateAvailable => Cow::Borrowed("No update found"),
            ComponentUpdatable::AtLatestVersion => Cow::Borrowed("At latest version"),
            ComponentUpdatable::WouldDowngrade => Cow::Borrowed("Ignoring downgrade"),
            ComponentUpdatable::Upgradable => Cow::Owned(format!(
                "Available: {}",
                component.update.as_ref().expect("update").version
            )),
        };
        println!("  Update: {}", msg);
    }

    if status.adoptable.is_empty() {
        println!("No components are adoptable.");
    }
    for (name, adopt) in status.adoptable.iter() {
        let ver = &adopt.version.version;
        if adopt.confident {
            println!("Detected: {}: {}", name, ver);
        } else {
            println!("Adoptable: {}: {}", name, ver);
        }
    }

    if let Some(coreos_aleph) = coreos::get_aleph_version()? {
        println!("CoreOS aleph image ID: {}", coreos_aleph.aleph.imgid);
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let boot_method = if Path::new("/sys/firmware/efi").exists() {
            "EFI"
        } else {
            "BIOS"
        };
        println!("Boot method: {}", boot_method);
    }

    Ok(())
}

pub(crate) fn client_run_update(c: &mut ipc::ClientToDaemonConnection) -> Result<()> {
    let status: Status = c.send(&ClientRequest::Status)?;
    if status.components.is_empty() && status.adoptable.is_empty() {
        println!("No components installed.");
        return Ok(());
    }
    let mut updated = false;
    for (name, cstatus) in status.components.iter() {
        match cstatus.updatable {
            ComponentUpdatable::Upgradable => {}
            _ => continue,
        };
        match c.send(&ClientRequest::Update {
            component: name.to_string(),
        })? {
            ComponentUpdateResult::AtLatestVersion => {
                // Shouldn't happen unless we raced with another client
                eprintln!(
                    "warning: Expected update for {}, raced with a different client?",
                    name
                );
                continue;
            }
            ComponentUpdateResult::Updated {
                previous: _,
                interrupted,
                new,
            } => {
                if let Some(i) = interrupted {
                    eprintln!(
                        "warning: Continued from previous interrupted update: {}",
                        i.version,
                    );
                }
                println!("Updated {}: {}", name, new.version);
            }
        }
        updated = true;
    }
    for (name, adoptable) in status.adoptable.iter() {
        if adoptable.confident {
            let r: ContentMetadata = c.send(&ClientRequest::AdoptAndUpdate {
                component: name.to_string(),
            })?;
            println!("Adopted and updated: {}: {}", name, r.version);
            updated = true;
        } else {
            println!("Component {} requires explicit adopt-and-update", name);
        }
    }
    if !updated {
        println!("No update available for any component.");
    }
    Ok(())
}

pub(crate) fn client_run_adopt_and_update(c: &mut ipc::ClientToDaemonConnection) -> Result<()> {
    let status: Status = c.send(&ClientRequest::Status)?;
    if status.adoptable.is_empty() {
        println!("No components are adoptable.");
    } else {
        for (name, _) in status.adoptable.iter() {
            let r: ContentMetadata = c.send(&ClientRequest::AdoptAndUpdate {
                component: name.to_string(),
            })?;
            println!("Adopted and updated: {}: {}", name, r.version);
        }
    }
    Ok(())
}

pub(crate) fn client_run_validate(c: &mut ipc::ClientToDaemonConnection) -> Result<()> {
    let status: Status = c.send(&ClientRequest::Status)?;
    if status.components.is_empty() {
        println!("No components installed.");
        return Ok(());
    }
    let mut caught_validation_error = false;
    for (name, _) in status.components.iter() {
        match c.send(&ClientRequest::Validate {
            component: name.to_string(),
        })? {
            ValidationResult::Valid => {
                println!("Validated: {}", name);
            }
            ValidationResult::Errors(errs) => {
                for err in errs {
                    eprintln!("{}", err);
                }
                caught_validation_error = true;
            }
        }
    }
    if caught_validation_error {
        anyhow::bail!("Caught validation errors");
    }
    Ok(())
}
