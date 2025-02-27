use crate::{prompt, users::get_all_users_keys};
use anyhow::{ensure, Context, Result};

const NO_SSH_PROMPT: &str = "None of the users on this system found have authorized SSH keys, \
    if your image doesn't use cloud-init or other means to set up users, \
    you may not be able to log in after reinstalling. Do you want to continue?";

fn prompt_single_user(user: &crate::users::UserKeys) -> Result<Vec<&crate::users::UserKeys>> {
    let prompt = indoc::formatdoc! {
        "Found only one user ({user}) with {num_keys} SSH authorized keys.
        Would you like to import its SSH authorized keys
        into the root user on the new bootc system?",
        user = user.user,
        num_keys = user.num_keys(),
    };
    let answer = ask_yes_no(&prompt, true)?;
    Ok(if answer { vec![&user] } else { vec![] })
}

fn prompt_user_selection(
    all_users: &[crate::users::UserKeys],
) -> Result<Vec<&crate::users::UserKeys>> {
    let keys: Vec<String> = all_users.iter().map(|x| x.user.clone()).collect();

    // TODO: Handle https://github.com/console-rs/dialoguer/issues/77
    let selected_user_indices: Vec<usize> = dialoguer::MultiSelect::new()
        .with_prompt(indoc::indoc! {
            "Select which user's SSH authorized keys you want to
            import into the root user of the new bootc system",
        })
        .items(&keys)
        .interact()?;

    Ok(selected_user_indices
        .iter()
        // Safe unwrap because we know the index is valid
        .map(|x| all_users.get(*x).unwrap())
        .collect())
}

/// Temporary safety mechanism to stop devs from running it on their dev machine. TODO: Discuss
/// final prompting UX in https://github.com/containers/bootc/discussions/1060
pub(crate) fn temporary_developer_protection_prompt() -> Result<()> {
    // Print an empty line so that the warning stands out from the rest of the output
    println!();

    let prompt = "THIS WILL REINSTALL YOUR SYSTEM! Are you sure you want to continue?";
    let answer = ask_yes_no(prompt, false)?;

    if !answer {
        println!("Exiting without reinstalling the system.");
        std::process::exit(0);
    }

    Ok(())
}

pub(crate) fn ask_yes_no(prompt: &str, default: bool) -> Result<bool> {
    dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(default)
        .wait_for_newline(true)
        .interact()
        .context("prompting")
}

/// Gather authorized keys for all user's of the host system
/// prompt the user to select which users's keys will be imported
/// into the target system's root user's authorized_keys file
///
/// The keys are stored in a temporary file which is passed to
/// the podman run invocation to be used by
/// `bootc install to-existing-root --root-ssh-authorized-keys`
pub(crate) fn get_ssh_keys(temp_key_file_path: &str) -> Result<()> {
    let users = get_all_users_keys()?;
    if users.is_empty() {
        ensure!(
            prompt::ask_yes_no(NO_SSH_PROMPT, false)?,
            "cancelled by user"
        );

        return Ok(());
    }

    let selected_users = if users.len() == 1 {
        prompt_single_user(&users[0])?
    } else {
        prompt_user_selection(&users)?
    };

    let keys = selected_users
        .into_iter()
        .flat_map(|user| &user.authorized_keys)
        .map(|key| {
            let mut key_copy = key.clone();

            // These options could contain a command which will
            // cause the new bootc system to be inaccessible.
            key_copy.options = None;
            key_copy.to_key_format() + "\n"
        })
        .collect::<String>();

    tracing::trace!("keys: {:?}", keys);

    std::fs::write(temp_key_file_path, keys.as_bytes())?;

    Ok(())
}
