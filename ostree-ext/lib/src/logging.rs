use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set to true if we failed to write to the journal once
static EMITTED_JOURNAL_ERROR: AtomicBool = AtomicBool::new(false);

/// Wrapper for systemd structured logging which only emits a message
/// if we're targeting the system repository, and it's booted.
pub(crate) fn system_repo_journal_send<K, V>(
    repo: &ostree::Repo,
    priority: libsystemd::logging::Priority,
    msg: &str,
    vars: impl Iterator<Item = (K, V)>,
) where
    K: AsRef<str>,
    V: AsRef<str>,
{
    if !libsystemd::daemon::booted() {
        return;
    }
    if !repo.is_system() {
        return;
    }
    if let Err(e) = libsystemd::logging::journal_send(priority, msg, vars) {
        if !EMITTED_JOURNAL_ERROR.swap(true, Ordering::SeqCst) {
            eprintln!("failed to write to journal: {e}");
        }
    }
}

/// Wrapper for systemd structured logging which only emits a message
/// if we're targeting the system repository, and it's booted.
pub(crate) fn system_repo_journal_print(
    repo: &ostree::Repo,
    priority: libsystemd::logging::Priority,
    msg: &str,
) {
    let vars: HashMap<&str, &str> = HashMap::new();
    system_repo_journal_send(repo, priority, msg, vars.into_iter())
}
