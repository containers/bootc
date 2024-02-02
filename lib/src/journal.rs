//! Thin wrapper for systemd journaling; these APIs are no-ops
//! when not running under systemd.  Only use them when

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set to true if we failed to write to the journal once
static EMITTED_JOURNAL_ERROR: AtomicBool = AtomicBool::new(false);

/// Wrapper for structured logging which is an explicit no-op
/// when systemd is not in use (e.g. in a container).
pub(crate) fn journal_send<K, V>(
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
    if let Err(e) = libsystemd::logging::journal_send(priority, msg, vars) {
        if !EMITTED_JOURNAL_ERROR.swap(true, Ordering::SeqCst) {
            eprintln!("failed to write to journal: {e}");
        }
    }
}

/// Wrapper for writing to systemd journal which is an explicit no-op
/// when systemd is not in use (e.g. in a container).
#[allow(dead_code)]
pub(crate) fn journal_print(priority: libsystemd::logging::Priority, msg: &str) {
    let vars: HashMap<&str, &str> = HashMap::new();
    journal_send(priority, msg, vars.into_iter())
}
