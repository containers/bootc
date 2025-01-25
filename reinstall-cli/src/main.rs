//! The main entrypoint for bootc-reinstall

use std::{io::Write, process::Command};

use anyhow::{Context, Result};

fn run() -> Result<()> {
    bootc_lib::cli::tracing_util::initialize_tracing();

    tracing::trace!("starting bootc-reinstall");

    prompt()?;

    let command_and_args = [
        // Rootless is not supported
        "sudo",
        // We use podman to run the bootc container. This might change in the future to remove the
        // podman dependency.
        "podman",
        "run",
        // The container needs to be privileged, as it heavily modifies the host
        "--privileged",
        // The container needs to access the host's PID namespace to mount host directories
        "--pid=host",
        // Since https://github.com/containers/bootc/pull/919 this mount should not be needed, but
        // some reason with e.g. quay.io/fedora/fedora-bootc:41 it is still needed.
        "-v",
        "/var/lib/containers:/var/lib/containers",
        // TODO: Get from argv
        "quay.io/fedora/fedora-bootc:41",
        // We're replacing the current root
        "bootc",
        "install",
        "to-existing-root",
        // The user already knows they're reinstalling their machine, that's the entire purpose of
        // this binary. Since this is no longer an "arcane" bootc command, we can safely avoid this
        // timed warning prompt. TODO: Discuss in https://github.com/containers/bootc/discussions/1060
        "--acknowledge-destructive",
    ];

    Command::new(command_and_args[0])
        .args(&command_and_args[1..])
        .status()
        .context(format!(
            "Failed to run the command \"{}\"",
            command_and_args.join(" ")
        ))?;

    Ok(())
}

/// Temporary safety mechanism to stop devs from running it on their dev machine. TODO: Discuss
/// final prompting UX in https://github.com/containers/bootc/discussions/1060
fn prompt() -> Result<(), anyhow::Error> {
    let prompt = "This will reinstall your system. Are you sure you want to continue? (y/n) ";
    let mut user_input = String::new();
    loop {
        print!("{}", prompt);
        std::io::stdout().flush()?;
        std::io::stdin().read_line(&mut user_input)?;
        match user_input.trim() {
            "y" => break,
            "n" => {
                println!("Exiting without reinstalling the system.");
                return Ok(());
            }
            _ => {
                println!("Unrecognized input. enter 'y' or 'n'.");
                user_input.clear();
            }
        }
    }
    Ok(())
}

fn main() {
    // In order to print the error in a custom format (with :#) our
    // main simply invokes a run() where all the work is done.
    // This code just captures any errors.
    if let Err(e) = run() {
        tracing::error!("{:#}", e);
        std::process::exit(1);
    }
}
