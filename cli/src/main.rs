// Good defaults
#![forbid(unused_must_use)]
#![deny(unsafe_code)]

use anyhow::Result;

async fn run() -> Result<()> {
    tracing_subscriber::fmt::init();
    tracing::trace!("starting");
    ostree_ext::cli::run_from_iter(std::env::args_os()).await
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}
