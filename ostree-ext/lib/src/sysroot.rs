//! Helpers for interacting with sysroots.

use std::ops::Deref;

use anyhow::Result;

/// A locked system root.
#[derive(Debug)]
pub struct SysrootLock {
    sysroot: ostree::Sysroot,
}

impl Drop for SysrootLock {
    fn drop(&mut self) {
        self.sysroot.unlock();
    }
}

impl Deref for SysrootLock {
    type Target = ostree::Sysroot;

    fn deref(&self) -> &Self::Target {
        &self.sysroot
    }
}

impl SysrootLock {
    /// Asynchronously acquire a sysroot lock.  If the lock cannot be acquired
    /// immediately, a status message will be printed to standard output.
    pub async fn new_from_sysroot(sysroot: &ostree::Sysroot) -> Result<Self> {
        let mut printed = false;
        loop {
            if sysroot.try_lock()? {
                return Ok(Self {
                    sysroot: sysroot.clone(),
                });
            }
            if !printed {
                println!("Waiting for sysroot lock...");
                printed = true;
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    }
}
