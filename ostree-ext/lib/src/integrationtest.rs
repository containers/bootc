//! Module used for integration tests; should not be public.

fn has_ostree() -> bool {
    std::path::Path::new("/sysroot/ostree/repo").exists()
}

pub(crate) fn detectenv() -> &'static str {
    match (crate::container_utils::running_in_container(), has_ostree()) {
        (true, true) => "ostree-container",
        (true, false) => "container",
        (false, true) => "ostree",
        (false, false) => "none",
    }
}
