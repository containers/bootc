//! Helpers related to tracing, used by main entrypoints

/// Initialize tracing with the default configuration.
pub fn initialize_tracing() {
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
        .with_max_level(tracing::Level::WARN)
        .init();
}
