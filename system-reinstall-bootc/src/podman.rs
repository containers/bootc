use std::process::Command;

use super::ROOT_KEY_MOUNT_POINT;
use crate::users::UserKeys;

pub(crate) fn command(image: &str, root_key: &Option<UserKeys>) -> Command {
    let mut podman_command_and_args = [
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
    ]
    .map(String::from)
    .to_vec();

    let mut bootc_command_and_args = [
        "bootc",
        "install",
        // We're replacing the current root
        "to-existing-root",
        // The user already knows they're reinstalling their machine, that's the entire purpose of
        // this binary. Since this is no longer an "arcane" bootc command, we can safely avoid this
        // timed warning prompt. TODO: Discuss in https://github.com/containers/bootc/discussions/1060
        "--acknowledge-destructive",
    ]
    .map(String::from)
    .to_vec();

    if let Some(root_key) = root_key.as_ref() {
        let root_authorized_keys_path = root_key.authorized_keys_path.clone();

        podman_command_and_args.push("-v".to_string());
        podman_command_and_args.push(format!(
            "{root_authorized_keys_path}:{ROOT_KEY_MOUNT_POINT}"
        ));

        bootc_command_and_args.push("--root-ssh-authorized-keys".to_string());
        bootc_command_and_args.push(ROOT_KEY_MOUNT_POINT.to_string());
    }

    let all_args = [
        podman_command_and_args,
        vec![image.to_string()],
        bootc_command_and_args,
    ]
    .concat();

    let mut command = Command::new(&all_args[0]);
    command.args(&all_args[1..]);

    command
}
