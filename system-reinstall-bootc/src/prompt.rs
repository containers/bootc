use crate::users::{get_all_users_keys, UserKeys};
use anyhow::{ensure, Context, Result};

fn prompt_single_user(user: &crate::users::UserKeys) -> Result<Vec<&crate::users::UserKeys>> {
    let prompt = format!(
        "Found only one user ({}) with {} SSH authorized keys. Would you like to import it and its keys to the system?",
        user.user,
        user.num_keys(),
    );
    let answer = ask_yes_no(&prompt, true)?;
    Ok(if answer { vec![&user] } else { vec![] })
}

fn prompt_user_selection(
    all_users: &[crate::users::UserKeys],
) -> Result<Vec<&crate::users::UserKeys>> {
    let keys: Vec<String> = all_users.iter().map(|x| x.user.clone()).collect();

    // TODO: Handle https://github.com/console-rs/dialoguer/issues/77
    let selected_user_indices: Vec<usize> = dialoguer::MultiSelect::new()
        .with_prompt("Select the users you want to install in the system (along with their authorized SSH keys)")
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

/// For now we only support the root user. This function returns the root user's SSH
/// authorized_keys. In the future, when bootc supports multiple users, this function will need to
/// be updated to return the SSH authorized_keys for all the users selected by the user.
pub(crate) fn get_root_key() -> Result<Option<UserKeys>> {
    let users = get_all_users_keys()?;
    if users.is_empty() {
        return Ok(None);
    }

    let selected_users = if users.len() == 1 {
        prompt_single_user(&users[0])?
    } else {
        prompt_user_selection(&users)?
    };

    ensure!(
        selected_users.iter().all(|x| x.user == "root"),
        "Only importing the root user keys is supported for now"
    );

    let root_key = selected_users.into_iter().find(|x| x.user == "root");

    Ok(root_key.cloned())
}
