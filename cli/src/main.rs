//! The main entrypoint for bootc, which just performs global initialization, and then
//! calls out into the library.

use anyhow::Result;

/// The code called after we've done process global init and created
/// an async runtime.
async fn async_main() -> Result<()> {
    // Don't include timestamps and such because they're not really useful and
    // too verbose, and plus several log targets such as journald will already
    // include timestamps.
    let format = tracing_subscriber::fmt::format()
        .without_time()
        .with_target(false)
        .compact();
    // Log to stderr by default
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .event_format(format)
        .with_writer(std::io::stderr)
        .init();
    tracing::trace!("starting");
    // As you can see, the role of this file is mostly to just be a shim
    // to call into the code that lives in the internal shared library.
    bootc_lib::cli::run_from_iter(std::env::args()).await
}

/// Perform process global initialization, then create an async runtime
/// and do the rest of the work there.
fn run() -> Result<()> {
    // Initialize global state before we've possibly created other threads, etc.
    bootc_lib::cli::global_init()?;
    // We only use the "current thread" runtime because we don't perform
    // a lot of CPU heavy work in async tasks. Where we do work on the CPU,
    // or we do want explicit concurrency, we typically use
    // tokio::task::spawn_blocking to create a new OS thread explicitly.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");
    // And invoke the async_main
    runtime.block_on(async move { async_main().await })
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
