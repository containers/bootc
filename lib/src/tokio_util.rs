//! Helpers for bridging GLib async/mainloop with Tokio.

use anyhow::Result;
use futures_util::Future;
use ostree::prelude::CancellableExt;

/// Call a faillible future, while monitoring `cancellable` and return an error if cancelled.
pub async fn run_with_cancellable<F, R>(f: F, cancellable: &ostree::gio::Cancellable) -> Result<R>
where
    F: Future<Output = Result<R>>,
{
    // Bridge GCancellable to a tokio notification
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let notify2 = notify.clone();
    cancellable.connect_cancelled(move |_| notify2.notify_one());
    cancellable.set_error_if_cancelled()?;
    tokio::select! {
       r = f => r,
       _ = notify.notified() => {
           Err(anyhow::anyhow!("Operation was cancelled"))
       }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cancellable() {
        let cancellable = ostree::gio::Cancellable::new();

        let cancellable_copy = cancellable.clone();
        let s = async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            cancellable_copy.cancel();
        };
        let r = async move {
            tokio::time::sleep(std::time::Duration::from_secs(200)).await;
            Ok(())
        };
        let r = run_with_cancellable(r, &cancellable);
        let (_, r) = tokio::join!(s, r);
        assert!(r.is_err());
    }
}
