use std::process::Command;

use once_cell::sync::Lazy;

pub(crate) const DEFAULT_UNPRIVILEGED_USER: &str = "nobody";

/// Checks if the current process is (apparently at least)
/// running under systemd.  We use this in various places
/// to e.g. log to the journal instead of printing to stdout.
pub(crate) fn running_in_systemd() -> bool {
    static RUNNING_IN_SYSTEMD: Lazy<bool> = Lazy::new(|| {
        // See https://www.freedesktop.org/software/systemd/man/systemd.exec.html#%24INVOCATION_ID
        std::env::var_os("INVOCATION_ID")
            .filter(|s| !s.is_empty())
            .is_some()
    });

    *RUNNING_IN_SYSTEMD
}

/// Return a prepared subprocess configuration that will run as an unprivileged user if possible.
///
/// This currently only drops privileges when run under systemd with DynamicUser.
pub(crate) fn unprivileged_subprocess(binary: &str, user: &str) -> Command {
    // TODO: if we detect we're running in a container as uid 0, perhaps at least switch to the
    // "bin" user if we can?
    if !running_in_systemd() {
        return Command::new(binary);
    }
    let mut cmd = Command::new("setpriv");
    // Clear some strategic environment variables that may cause the containers/image stack
    // to look in the wrong places for things.
    cmd.env_remove("HOME");
    cmd.env_remove("XDG_DATA_DIR");
    cmd.env_remove("USER");
    cmd.args([
        "--no-new-privs",
        "--init-groups",
        "--reuid",
        user,
        "--bounding-set",
        "-all",
        "--pdeathsig",
        "TERM",
        "--",
        binary,
    ]);
    cmd
}
