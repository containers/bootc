//! This module contains logic to "take over" a system by moving the running
//! container into RAM, then switching root to it (becoming PID 1).
//!

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Write};
use std::process::Command;
use std::sync::Arc;
use std::{io::BufWriter, os::unix::process::CommandExt};

use anyhow::{Context, Result};
use camino::Utf8Path;
use cap_std::fs::{Dir, DirBuilder};
use cap_std_ext::cap_std;
use cap_std_ext::prelude::CapStdExtDirExt;
use fn_error_context::context;
use ostree_ext::container as ostree_container;
use ostree_ext::ostree;
use ostree_ext::ostree::gio;
use rustix::fd::{AsFd, AsRawFd};
use rustix::process::getpid;
use serde::{Deserialize, Serialize};

use crate::install::baseline::InstallBlockDeviceOpts;
use crate::install::{SourceInfo, State};
use crate::task::Task;
use crate::utils::run_in_host_mountns;

const BOOTC_RUNDIR: &str = "bootc";
/// The system global path we move ourself into
const MEMORY_ROOT: &str = "tmp-root";
/// The subdirectory of the memory root with the ostree repo
const SYSROOT: &str = "sysroot";
/// The subdirectory of the memory root which has our new root
const ROOTDIR: &str = "rootfs";
/// The file path in the root where we serialize the install options struct
const STATE_PATH: &str = "install-state.json";

/// The filesystem name we use as a hard link to know we're running as init
pub(crate) const BIN_NAME: &str = "bootc-install-from-memory";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct SerializedState {
    source_imageref: String,
    source_digest: String,
    config: crate::install::config::InstallConfiguration,
    commit: String,
    selinux: bool,
}

impl SerializedState {
    /// The file path in the root where we write SerializedState
    const PATH: &str = "takeover-source.json";
}

/// Global state for takeover process
pub(crate) struct RunContext {
    console: BufWriter<File>,
}

// This is somewhat similar to what we do for installing to a target root;
// pull the data from containers-storage and synthesize an ostree commit,
// but instead of doing a full deployment, just do a pull and then check out.
// A full deployment is like a checkout but also updates the bootloader config
// and has concepts like a "stateroot" etc. that we don't need here.
#[context("Copying self")]
pub(crate) async fn copy_self_to(source: &SourceInfo, target: &Dir) -> Result<()> {
    use ostree_container::store::PrepareResult;
    let cancellable = gio::Cancellable::NONE;
    let self_imgref = ostree_container::OstreeImageReference {
        // There are no signatures to verify since we're fetching the already
        // pulled container.
        sigverify: ostree_container::SignatureSource::ContainerPolicyAllowInsecure,
        imgref: source.imageref.clone(),
    };

    tracing::debug!("Preparing import from {self_imgref}");
    // We need to fetch the container image from the root mount namespace
    let proxy_cfg = crate::install::import_config_from_host();
    let repo_path = &Utf8Path::new(SYSROOT).join("repo");
    let mut db = DirBuilder::new();
    db.recursive(true);
    target
        .ensure_dir_with(repo_path, &db)
        .with_context(|| format!("Creating {repo_path}"))?;
    let repo = &ostree::Repo::create_at_dir(target.as_fd(), "repo", ostree::RepoMode::Bare, None)
        .context("Creating repo")?;
    repo.set_disable_fsync(true);
    let mut imp = ostree_container::store::ImageImporter::new(&repo, &self_imgref, proxy_cfg)
        .await
        .context("Initializing importer")?;
    let img = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(i) => i,
        PrepareResult::Ready(r) => imp.import(r).await?,
    };

    let commit = img.get_commit();
    tracing::debug!("Imported {commit}");

    target
        .remove_all_optional(ROOTDIR)
        .context("Cleaning up {ROOTDIR}")?;

    // We've imported the container as an ostree commit.
    // Now check out the filesystem tree.
    tokio::task::spawn_blocking({
        let repo = repo.clone();
        let target = target.try_clone()?;
        let commit = commit.to_owned();
        let cancellable = cancellable.clone();
        move || {
            let checkout_opts = ostree::RepoCheckoutAtOptions {
                mode: ostree::RepoCheckoutMode::None,
                no_copy_fallback: true,
                force_copy_zerosized: true,
                enable_fsync: false,
                ..Default::default()
            };
            tracing::debug!("Performing checkout");
            repo.checkout_at(
                Some(&checkout_opts),
                target.as_raw_fd(),
                ROOTDIR,
                &commit,
                cancellable,
            )
        }
    })
    .await
    .context("Performing checkout")??;

    // This special hardlink signals the main code that we're in takeover mode.
    let rootdir = target.open_dir(ROOTDIR)?;
    rootdir
        .hard_link(
            "usr/bin/bootc",
            &rootdir,
            Utf8Path::new("usr/bin").join(BIN_NAME),
        )
        .with_context(|| format!("Hard linking to {BIN_NAME}"))?;
    tracing::debug!("Checkout OK");

    Ok(())
}

/// Prepare mounts in the target root before we switch
#[context("Setting up target root")]
fn setup_target_root(root: &Dir) -> Result<()> {
    // Mount /sysroot in the target root so we can see the ostree repo
    let target_sysroot = Utf8Path::new(ROOTDIR).join("sysroot");
    Task::new(format!("Bind mount /sysroot"), "mount")
        .cwd(root)?
        .args(["--bind", "sysroot", target_sysroot.as_str()])
        .run()?;
    Ok(())
}

#[context("Re-executing to perform install from memory")]
pub(crate) async fn run_from_host(opts: InstallBlockDeviceOpts, state: Arc<State>) -> Result<()> {
    let host_runpath = super::install::HOST_RUNDIR;
    let host_run = Dir::open_ambient_dir(host_runpath, cap_std::ambient_authority())
        .with_context(|| format!("Failed to open {host_runpath}"))?;
    let global_rundir = host_run.open_dir("run").context("Opening host /run")?;
    global_rundir
        .create_dir_all(BOOTC_RUNDIR)
        .with_context(|| format!("Creating {BOOTC_RUNDIR}"))?;
    let rundir = global_rundir.open_dir(BOOTC_RUNDIR)?;
    // Copy the container to /run/bootc/tmp-root
    rundir
        .create_dir_all(MEMORY_ROOT)
        .with_context(|| format!("Creating {MEMORY_ROOT}"))?;
    let target = rundir
        .open_dir(MEMORY_ROOT)
        .with_context(|| format!("Opening {MEMORY_ROOT}"))?;
    tracing::debug!("Writing to {MEMORY_ROOT}");

    copy_self_to(&state.source, &target).await?;
    // Prepare mounts in the new temporary root
    setup_target_root(&target)?;
    tracing::debug!("Set up target root");

    // Serialize the install data into /run so we can pick it up when we re-execute
    rundir
        .atomic_replace_with(STATE_PATH, move |w| {
            serde_json::to_writer(w, &opts)?;
            anyhow::Ok(())
        })
        .context("Writing serialized options")?;
    // Serialize the container source into /run too
    rundir
        .atomic_replace_with(SerializedState::PATH, move |w| {
            let state = SerializedState {
                source_imageref: state.source.imageref.to_string(),
                source_digest: state.source.digest.clone(),
                config: state.install_config.clone(),
                commit: state.source.commit.clone(),
                selinux: state.source.selinux,
            };
            serde_json::to_writer(w, &state)?;
            anyhow::Ok(())
        })
        .context("Writing serialized state")?;

    if dbg!(std::env::var_os("bootc_exit").is_some()) {
        return Ok(());
    }

    // Systemd tries to reload policy in the new root, which fails because policy is already
    // loaded; we don't want that here.  So for takeover installs, let's just set permissive mode.
    crate::lsm::selinux_set_permissive(true)?;
    tracing::debug!("Invoking systemctl switch-root");
    let abs_target_root = format!("/run/{BOOTC_RUNDIR}/{MEMORY_ROOT}/{ROOTDIR}");
    let bin = format!("/usr/bin/{BIN_NAME}");
    // Then, systemctl switch-root to our target root, re-executing our own
    // binary.
    // We will likely want to accept things like a log file path (and support remote logging)
    // so that we can easily debug the install process if it fails in the middle.
    Task::new_cmd(
        "Invoking systemctl switch-root",
        run_in_host_mountns("systemctl"),
    )
    .args(["switch-root", abs_target_root.as_str(), bin.as_str()])
    .run()?;

    println!("Waiting for termination of this process...");
    // 5 minutes should be long enough for practical purposes
    std::thread::sleep(std::time::Duration::from_secs(5 * 60));

    anyhow::bail!("Failed to wait for systemctl switch-root");
}

async fn run_impl(ctx: &mut RunContext) -> Result<()> {
    anyhow::ensure!(getpid().is_init());

    let _ = writeln!(ctx.console, "bootc: Preparing takeover installation");

    let global_rundir = Dir::open_ambient_dir("/run", cap_std::ambient_authority())?;

    // Deserialize our system state
    let opts = {
        let f = global_rundir
            .open(STATE_PATH)
            .map(BufReader::new)
            .with_context(|| format!("Opening {STATE_PATH}"))?;
        let mut opts: crate::install::InstallOpts = serde_json::from_reader(f)?;
        // This must be set if we got here
        anyhow::ensure!(opts.block_opts.takeover);
        // But avoid infinite recursion =)
        opts.block_opts.takeover = false;
        opts
    };
    let serialized_state: SerializedState = {
        let f = global_rundir
            .open(SerializedState::PATH)
            .map(BufReader::new)
            .with_context(|| format!("Opening {}", SerializedState::PATH))?;
        serde_json::from_reader(f).context("Parsing serialized state")?
    };

    let state = State {
        source: SourceInfo {
            imageref: serialized_state.source_imageref.as_str().try_into()?,
            digest: serialized_state.source_digest,
            from_ostree_repo: true,
            commit: serialized_state.commit,
            selinux: serialized_state.selinux,
        },
        override_disable_selinux: opts.config_opts.disable_selinux,
        config_opts: opts.config_opts,
        target_opts: opts.target_opts,
        install_config: serialized_state.config,
        setenforce_guard: None,
    };
    let state = Arc::new(state);

    // Now, we perform the install to the target block device.  We should be
    // pretty confident that all prior mounts were torn down (systemd should
    // have done this, but see above for using `systemctl isolate` to help make
    // sure.

    // TODO: In this model we already have the ostree commit pulled.  Let's
    // refactor the install code to also have a "from pre-pulled ostree commit"
    // path too.
    crate::install::install_takeover(opts.block_opts, state).await?;
    Ok(())
}

/// There's not much we can do if something went wrong as pid 1.
/// Write the error to the console, then reboot.
fn handle_fatal_error(ctx: &mut RunContext, e: anyhow::Error) -> ! {
    // Best effort to write output to the console
    let _ = writeln!(ctx.console, "{e}");
    let _ = ctx.console.flush();

    // There is definitely going to be something better to do here,
    // but it entirely depends on at which point we fail.  Roughly
    // likely something like this:
    //
    // - Before we run sgdisk to rewrite the target blockdev:
    //   We can write to a log file that persists, the host should still
    //   be fine on the next boot.
    // - After we've run sgdisk, but before the deployment completes:
    //   Perhaps try to rewrite the partition tables again and at least
    //   record the error to a temporary partition.  (Or we could
    //   *always* log to a temporary partition, but then remove it just
    //   before rebooting?)
    std::thread::sleep(std::time::Duration::from_secs(60));
    reboot()
}

fn reboot() -> ! {
    let e = Command::new("reboot").arg("-ff").exec();
    panic!("Failed to exec reboot: {e}");
}

// Because we're running as pid1, exiting will cause a kernel panic; so we don't.
pub(crate) async fn run() -> ! {
    // At this point, we're running as pid1.  We could fork ourself and run
    // the real work as a child process if that helped things, but eh.

    let console = match OpenOptions::new()
        .write(true)
        .open("/dev/console")
        .map(BufWriter::new)
    {
        Ok(c) => c,
        Err(e) => {
            panic!("Failed to open /dev/console: {e}")
        }
    };
    let mut ctx = RunContext { console };

    match run_impl(&mut ctx).await {
        Ok(()) => {
            let _ = writeln!(ctx.console, "Rebooting");
            println!("Rebooting");
            reboot()
        }
        Err(e) => handle_fatal_error(&mut ctx, e),
    }
}
