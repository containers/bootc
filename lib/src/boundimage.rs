use anyhow::Result;

use ostree_ext::ostree::Deployment;
use ostree_ext::sysroot::SysrootLock;

use std::fs;
use std::path::Path;

use crate::task::Task;

use tempfile::TempDir;

const BOOTC_QUADLET_DIR: &'static str = "/etc/containers/systemd/bootc";
const QUADLET_BINARY: &'static str = "/usr/lib/systemd/system-generators/podman-system-generator";
const SYSTEMD_DIR: &'static str = "/etc/systemd/system";

pub(crate) struct BoundImageManager {
    quadlet_unit_dir: String,
    units: Vec<String>,
    temp_dir: TempDir,
}

impl BoundImageManager {
    pub(crate) fn new(deployment: &Deployment, sysroot: &SysrootLock) -> Result<BoundImageManager> {
        let deployment_dir = sysroot.deployment_dirpath(&deployment);
        let quadlet_unit_dir = format!("/{deployment_dir}/{BOOTC_QUADLET_DIR}");

        let temp_dir = TempDir::new()?;
        let bound_image_manager = BoundImageManager {
            quadlet_unit_dir,
            units: Vec::new(),
            temp_dir,
        };
        Ok(bound_image_manager)
    }

    pub(crate) fn run(&mut self) -> Result<()> {
        if Path::new(&self.quadlet_unit_dir).exists() {
            self.run_quadlet()?;
            self.move_units()?;
            self.restart_systemd()?;
            self.start_new_services()?;
        }

        Ok(())
    }

    // Run podman-system-generator to generate the systemd units
    // the output is written to a temporary directory
    // in order to track the generated units.
    // The generated units need to be moved to /etc/systemd/system
    // to be started by systemd.
    fn run_quadlet(&self) -> Result<()> {
        Task::new(
            format!("Running quadlet on {:#}", self.quadlet_unit_dir),
            QUADLET_BINARY,
        )
        .arg(self.temp_dir.path())
        .env(&"QUADLET_UNIT_DIRS".to_string(), &self.quadlet_unit_dir)
        .run()?;

        Ok(())
    }

    fn move_units(&mut self) -> Result<()> {
        let entries = fs::read_dir(self.temp_dir.path())?;
        for bound_image in entries {
            let bound_image = bound_image?;
            let bound_image_path = bound_image.path();
            let unit_name = bound_image_path.file_name().unwrap().to_str().unwrap();

            //move the unit file from the bootc subdirectory to the root systemd directory
            let systemd_dst = format!("{SYSTEMD_DIR}/{unit_name}");
            if !Path::new(systemd_dst.as_str()).exists() {
                fs::copy(&bound_image_path, systemd_dst)?;
            }

            self.units.push(unit_name.to_string());
        }

        Ok(())
    }

    fn restart_systemd(&self) -> Result<()> {
        Task::new_and_run("Reloading systemd", "/usr/bin/systemctl", ["daemon-reload"])?;
        Ok(())
    }

    fn start_new_services(&self) -> Result<()> {
        //TODO: do this in parallel
        for unit in &self.units {
            Task::new_and_run(
                format!("Starting target: {:#}", unit),
                "/usr/bin/systemctl",
                ["start", unit],
            )?;
        }
        Ok(())
    }
}

impl Drop for BoundImageManager {
    //remove the generated units from the root systemd directory
    //and stop them to remove the services from systemd
    fn drop(&mut self) {
        for unit in &self.units {
            //TODO: error handling
            let _ = fs::remove_file(format!("{SYSTEMD_DIR}/{unit}"));
            let _ = Task::new_and_run(
                format!("Starting target: {:#}", unit),
                "/usr/bin/systemctl",
                ["stop", unit],
            );
        }
    }
}
