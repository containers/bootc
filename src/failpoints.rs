//! Wrappers and utilities on top of the `fail` crate.
// SPDX-License-Identifier: Apache-2.0 OR MIT

/// TODO: Use https://github.com/tikv/fail-rs/pull/68 once it merges
/// copy from https://github.com/coreos/rpm-ostree/commit/aa8d7fb0ceaabfaf10252180e2ddee049d07aae3#diff-adcc419e139605fae34d17b31418dbaf515af2fe9fb766fcbdb2eaad862b3daa
#[macro_export]
macro_rules! try_fail_point {
    ($name:expr) => {{
        if let Some(e) = fail::eval($name, |msg| {
            let msg = msg.unwrap_or_else(|| "synthetic failpoint".to_string());
            anyhow::Error::msg(msg)
        }) {
            return Err(From::from(e));
        }
    }};
    ($name:expr, $cond:expr) => {{
        if $cond {
            $crate::try_fail_point!($name);
        }
    }};
}
