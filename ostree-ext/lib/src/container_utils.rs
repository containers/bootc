//! Helpers for interacting with containers at runtime.

/// Attempts to detect if the current process is running inside a container.
/// This looks for the `container` environment variable or the presence
/// of Docker or podman's more generic `/run/.containerenv`.
pub fn running_in_container() -> bool {
    if std::env::var_os("container").is_some() {
        return true;
    }
    // https://stackoverflow.com/questions/20010199/how-to-determine-if-a-process-runs-inside-lxc-docker
    for p in ["/run/.containerenv", "/.dockerenv"] {
        if std::path::Path::new(p).exists() {
            return true;
        }
    }
    false
}
