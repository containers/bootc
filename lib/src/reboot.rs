//! Handling of system restarts/reboot

use std::io::Write;
use std::process::Command;

use fn_error_context::context;

/// Initiate a system reboot.
/// This function will only return in case of error.
#[context("Initiating reboot")]
pub(crate) fn reboot() -> anyhow::Result<()> {
    // Flush output streams
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    let st = Command::new("reboot").status()?;
    if !st.success() {
        anyhow::bail!("Failed to reboot: {st:?}");
    }
    tracing::debug!("Initiated reboot, sleeping forever...");
    loop {
        std::thread::park();
    }
}
