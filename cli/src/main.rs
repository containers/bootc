use anyhow::Result;

async fn run() -> Result<()> {
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
    bootc_lib::cli::run_from_iter(std::env::args()).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        tracing::error!("{:#}", e);
        std::process::exit(1);
    }
}
