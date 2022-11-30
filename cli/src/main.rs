// Good defaults
#![forbid(unused_must_use)]
#![deny(unsafe_code)]

use anyhow::Result;

async fn run() -> Result<()> {
    tracing_subscriber::fmt::init();
    tracing::trace!("starting");
    bootc_lib::cli::run_from_iter(std::env::args()).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}
